import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import { handleRunRoute } from "../src/routes/runs";
import type { AuthContext } from "../src/auth";
import { HttpError } from "../src/http";
import type { PackageRepository, RunAttemptRecord, RunEventRecord, RunRepository } from "../src/packages/repository";
import type { JsonObject } from "../src/primitives";
import type { RuntimeRepository } from "../src/runtime/repository";

const RUN_ID = "33333333-3333-4333-8333-333333333333";
const ATTEMPT_ID = "44444444-4444-4444-8444-444444444444";
const RUNNER_INSTANCE_ID = "55555555-5555-4555-8555-555555555555";

describe("Cloud run routes", () => {
  test("redacts env values and secret refs from user-facing run list responses", async () => {
    const runs = {
      async listRuns() {
        return [runRecord({
          error_message:
            "runner failed in /Users/alice/private/run token=raw-run-token file:///private/tmp/run.log",
          runtime_options: {
            debug_path: "/Users/alice/private/run/debug.json",
            access_token: "raw-runtime-option-token",
            network: {
              egress: ["api.openai.com"],
            },
          },
          run_requirements: {
            image_refs: [
              "ghcr.io/acme/runtime@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "https://registry.example/acme/runtime?token=raw-image-token",
            ],
          },
        })];
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs"),
      new URL("https://cloud.example/v1/runs"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    expect(body.runs[0].env).toBeUndefined();
    expect(body.runs[0].secret_refs).toBeUndefined();
    expect(body.runs[0].env_keys).toEqual(["PUBLIC_FLAG", "SENSITIVE_ENV"]);
    expect(body.runs[0].secret_ids).toEqual(["OPENAI_API_KEY"]);
    expect(body.runs[0].error_message).toContain("[redacted-local-path]");
    expect(body.runs[0].error_message).toContain("token=[redacted-secret]");
    expect(body.runs[0].error_message).toContain("file://[redacted-local-path]");
    expect(body.runs[0].runtime_options.debug_path).toBe("[redacted-local-path]");
    expect(body.runs[0].runtime_options.access_token).toBe("[redacted]");
    expect(body.runs[0].runtime_options.network).toEqual({ egress: ["api.openai.com"] });
    expect(body.runs[0].run_requirements.secret_ids).toEqual(["OPENAI_API_KEY"]);
    expect(body.runs[0].run_requirements.image_refs[1]).toContain("[redacted URL credentials/query]");
    expect(JSON.stringify(body)).not.toContain("secret-env-value");
    expect(JSON.stringify(body)).not.toContain("projects/acme/secrets/openai");
    expect(JSON.stringify(body)).not.toContain("/Users/alice");
    expect(JSON.stringify(body)).not.toContain("/private/tmp");
    expect(JSON.stringify(body)).not.toContain("raw-run-token");
    expect(JSON.stringify(body)).not.toContain("raw-runtime-option-token");
    expect(JSON.stringify(body)).not.toContain("raw-image-token");
  });

  test("runtime observation responses redact repository output at the public boundary", async () => {
    const runs = {
      async getRun() {
        return runRecord();
      },
    };
    const runtime = {
      async getSummary() {
        return {
          cloud_run_id: RUN_ID,
          core_run_ids: ["core-1"],
          runtime_snapshots: [{
            core_run_id: "core-1",
            run_dir_name: "core-1",
            runtime_values: {
              run_session_state_v1: {
                project_root: "/Users/alice/private/project",
                access_token: "raw-runtime-token",
              },
            },
            trial_summaries: [{
              trial_id: "trial-1",
              summary: {
                log_url: "file:///private/tmp/bucephalus/summary.log",
                api_key: "sk-abcdefghijklmnopqrstuvwxyz",
              },
            }],
            evidence_records: [],
            omitted: ["/private/tmp/bucephalus/large-events.jsonl"],
          }],
          run_controls: [{
            run_root: "/Users/alice/private/run",
            token_hint: "raw-control-token",
          }],
          schedule_progress: [],
          active_slots: [{
            slot: {
              worker_path: "/home/alice/worker/slot.json",
            },
          }],
          recent_events: [{
            payload: {
              message: "event failed in /Users/alice/private/event token=raw-summary-event-token",
            },
            row: {
              stderr: "file:///private/tmp/bucephalus/event.log",
            },
          }],
        };
      },
      async eventRows() {
        return [{
          payload: {
            message: "stream event failed in /Users/alice/private/event token=raw-runtime-event-token",
          },
          row: {
            log_url: "file:///private/tmp/bucephalus/event.log",
            nested: {
              api_key: "sk-abcdefghijklmnopqrstuvwxyz",
            },
          },
        }];
      },
      async results() {
        return {
          cloud_run_id: RUN_ID,
          core_run_ids: ["core-1"],
          trial_results: [{
            row: {
              artifact_path: "/private/tmp/bucephalus/result.json",
              access_token: "raw-result-token",
            },
          }],
          metric_observations: [{
            row: {
              metric_path: "/Users/alice/private/metric.json",
            },
          }],
          contract_stages: [{
            detail: {
              message: "contract detail in /home/alice/project token=raw-contract-token",
            },
          }],
          attempt_objects: [{
            object_ref: "file:///private/tmp/bucephalus/object.json",
            metadata: {
              password: "raw-object-password",
            },
          }],
        };
      },
      async runtimeValue() {
        return [{
          project_root: "/Users/alice/private/project",
          secret_ref: "gcp-secret-manager://projects/acme/secrets/openai/versions/1",
          api_key: "sk-abcdefghijklmnopqrstuvwxyz",
        }];
      },
    };

    for (const path of [
      `/v1/runs/${RUN_ID}/runtime`,
      `/v1/runs/${RUN_ID}/runtime/events`,
      `/v1/runs/${RUN_ID}/runtime/results`,
      `/v1/runs/${RUN_ID}/runtime/kv/run_session_state_v1`,
    ]) {
      const response = await handleRunRoute(
        new Request(`https://cloud.example${path}`),
        new URL(`https://cloud.example${path}`),
        {} as PackageRepository,
        runs as unknown as RunRepository,
        runtime as unknown as RuntimeRepository,
        "worker-token",
        authContext("user-a"),
      );

      expect(response).not.toBeNull();
      const encoded = JSON.stringify(await response!.json());
      expect(encoded).toContain("[redacted");
      expect(encoded).not.toContain("/Users/alice");
      expect(encoded).not.toContain("/private/tmp");
      expect(encoded).not.toContain("/home/alice");
      expect(encoded).not.toContain("raw-runtime-token");
      expect(encoded).not.toContain("raw-control-token");
      expect(encoded).not.toContain("raw-summary-event-token");
      expect(encoded).not.toContain("raw-runtime-event-token");
      expect(encoded).not.toContain("raw-result-token");
      expect(encoded).not.toContain("raw-contract-token");
      expect(encoded).not.toContain("raw-object-password");
      expect(encoded).not.toContain("gcp-secret-manager://");
      expect(encoded).not.toContain("sk-abcdefghijklmnopqrstuvwxyz");
    }
  });

  test("runtime observation query params fail clearly before repository calls", async () => {
    let runtimeCalls = 0;
    const runs = {
      async getRun() {
        return runRecord();
      },
    };
    const runtime = {
      async eventRows() {
        runtimeCalls += 1;
        return [];
      },
      async results() {
        runtimeCalls += 1;
        return {};
      },
    };

    for (const [path, param, message] of [
      [
        `/v1/runs/${RUN_ID}/runtime/events?limit=token=raw-limit-secret`,
        "limit",
        "limit must be an integer from 1 to 1000",
      ],
      [
        `/v1/runs/${RUN_ID}/runtime/events?after_row_seq=-1`,
        "after_row_seq",
        "after_row_seq must be an integer from 0 to 9007199254740991",
      ],
      [
        `/v1/runs/${RUN_ID}/runtime/results?limit=1001`,
        "limit",
        "limit must be an integer from 1 to 1000",
      ],
    ] as const) {
      let caught: unknown;
      try {
        await handleRunRoute(
          new Request(`https://cloud.example${path}`),
          new URL(`https://cloud.example${path}`),
          {} as PackageRepository,
          runs as unknown as RunRepository,
          runtime as unknown as RuntimeRepository,
          "worker-token",
          authContext("user-a"),
        );
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(HttpError);
      const error = caught as HttpError;
      expect(error.status).toBe(400);
      expect(error.code).toBe("invalid_query_param");
      expect(error.message).toBe(message);
      expect(error.detail).toEqual({
        param,
        min: param === "after_row_seq" ? 0 : 1,
        max: param === "after_row_seq" ? Number.MAX_SAFE_INTEGER : 1000,
      });
      expect(JSON.stringify({
        message: error.message,
        detail: error.detail,
      })).not.toContain("raw-limit-secret");
    }
    expect(runtimeCalls).toBe(0);
  });

  test("runtime observation paths fail clearly on malformed encoding before repository calls", async () => {
    let runCalls = 0;
    let runtimeCalls = 0;
    const runs = {
      async getRun() {
        runCalls += 1;
        return runRecord();
      },
    };
    const runtime = {
      async eventRows() {
        runtimeCalls += 1;
        return [];
      },
    };

    let caught: unknown;
    try {
      await handleRunRoute(
        new Request("https://cloud.example/v1/runs/%E0%A4%A-token=raw-run-path-secret/runtime/events"),
        new URL("https://cloud.example/v1/runs/%E0%A4%A-token=raw-run-path-secret/runtime/events"),
        {} as PackageRepository,
        runs as unknown as RunRepository,
        runtime as unknown as RuntimeRepository,
        "worker-token",
        authContext("user-a"),
      );
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(HttpError);
    const error = caught as HttpError;
    expect(error.status).toBe(400);
    expect(error.code).toBe("invalid_path_param");
    expect(error.message).toBe("/run_id must be valid percent-encoded UTF-8");
    expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain("raw-run-path-secret");
    expect(runCalls).toBe(0);
    expect(runtimeCalls).toBe(0);
  });

  test("run paths reject malformed run ids before repository calls", async () => {
    let runCalls = 0;
    let runtimeCalls = 0;
    const runs = {
      async getRun() {
        runCalls += 1;
        return runRecord();
      },
    };
    const runtime = {
      async getSummary() {
        runtimeCalls += 1;
        return {};
      },
      async eventRows() {
        runtimeCalls += 1;
        return [];
      },
    };

    for (const [path, rawSecret] of [
      [
        "/v1/runs/token=raw-run-id-secret",
        "raw-run-id-secret",
      ],
      [
        "/v1/runs/%2FUsers%2Falice%2Fprivate%2Frun/runtime",
        "/Users/alice",
      ],
      [
        "/v1/runs/not-a-uuid/runtime/events",
        "not-a-uuid",
      ],
    ] as const) {
      let caught: unknown;
      try {
        await handleRunRoute(
          new Request(`https://cloud.example${path}`),
          new URL(`https://cloud.example${path}`),
          {} as PackageRepository,
          runs as unknown as RunRepository,
          runtime as unknown as RuntimeRepository,
          "worker-token",
          authContext("user-a"),
        );
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(HttpError);
      const error = caught as HttpError;
      expect(error.status).toBe(400);
      expect(error.code).toBe("invalid_request");
      expect(error.message).toBe("/run_id must be a UUID");
      expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain(rawSecret);
    }
    expect(runCalls).toBe(0);
    expect(runtimeCalls).toBe(0);
  });

  test("run list limit query params fail before listing", async () => {
    let listCalls = 0;
    const runs = {
      async listRuns() {
        listCalls += 1;
        return [];
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs?limit=201"),
      new URL("https://cloud.example/v1/runs?limit=201"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("limit must be an integer from 1 to 200");
    expect(listCalls).toBe(0);
  });

  test("package digest inputs fail clearly before repository calls", async () => {
    let packageCalls = 0;
    let attemptTokenCalls = 0;
    const packages = {
      async getArtifact() {
        packageCalls += 1;
        return null;
      },
    };
    const runs = {
      async verifyAttemptToken() {
        attemptTokenCalls += 1;
      },
    };

    for (const [request, url] of [
      [
        new Request("https://cloud.example/v1/packages/sha256:not-hex-token=raw-package-secret"),
        new URL("https://cloud.example/v1/packages/sha256:not-hex-token=raw-package-secret"),
      ],
      [
        new Request("https://cloud.example/v1/packages/sha256:not-hex-token=raw-content-secret/content", {
          headers: {
            authorization: "Bearer attempt-token",
            "x-bucephalus-attempt-id": ATTEMPT_ID,
          },
        }),
        new URL("https://cloud.example/v1/packages/sha256:not-hex-token=raw-content-secret/content"),
      ],
      [
        new Request("https://cloud.example/v1/runs", {
          method: "POST",
          body: JSON.stringify({
            package_digest: "sha256:not-hex-token=raw-body-secret",
          }),
        }),
        new URL("https://cloud.example/v1/runs"),
      ],
    ] as const) {
      let caught: unknown;
      try {
        await handleRunRoute(
          request,
          url,
          packages as unknown as PackageRepository,
          runs as unknown as RunRepository,
          {} as RuntimeRepository,
          "worker-token",
          authContext("user-a"),
        );
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(HttpError);
      const error = caught as HttpError;
      expect(error.status).toBe(400);
      expect(error.code).toBe("invalid_digest");
      expect(error.message).toBe("/package_digest must be sha256:<64 lowercase hex chars>");
      const encoded = JSON.stringify({ message: error.message, detail: error.detail });
      expect(encoded).not.toContain("raw-package-secret");
      expect(encoded).not.toContain("raw-content-secret");
      expect(encoded).not.toContain("raw-body-secret");
    }
    expect(packageCalls).toBe(0);
    expect(attemptTokenCalls).toBe(0);
  });

  test("worker route ids fail clearly before repository calls", async () => {
    const digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const calls: string[] = [];
    const packages = {
      async getArtifact() {
        calls.push("getArtifact");
        return null;
      },
    };
    const runs = {
      async claimNextRun() {
        calls.push("claimNextRun");
        return null;
      },
      async verifyAttemptToken() {
        calls.push("verifyAttemptToken");
      },
      async heartbeatAttempt() {
        calls.push("heartbeatAttempt");
        return attemptRecord();
      },
      async appendRunEvent() {
        calls.push("appendRunEvent");
        return eventRecord();
      },
    };

    const cases: Array<{
      request: Request;
      url: URL;
      code: string;
      message: string;
      forbidden: string;
    }> = [
      {
        request: new Request("https://cloud.example/v1/worker/runs/claim", {
          method: "POST",
          headers: {
            authorization: "Bearer worker-token",
            "content-type": "application/json",
          },
          body: JSON.stringify({
            runner_instance_id: "token=raw-claim-runner-secret /Users/alice/private/runner",
          }),
        }),
        url: new URL("https://cloud.example/v1/worker/runs/claim"),
        code: "invalid_request",
        message: "/runner_instance_id must be a UUID",
        forbidden: "raw-claim-runner-secret",
      },
      {
        request: new Request("https://cloud.example/v1/worker/run-attempts/not-a-uuid/heartbeat", {
          method: "POST",
          headers: {
            authorization: "Bearer attempt-token",
            "content-type": "application/json",
          },
          body: JSON.stringify({
            runner_instance_id: RUNNER_INSTANCE_ID,
          }),
        }),
        url: new URL("https://cloud.example/v1/worker/run-attempts/not-a-uuid/heartbeat"),
        code: "invalid_request",
        message: "/attempt_id must be a UUID",
        forbidden: "not-a-uuid",
      },
      {
        request: new Request(`https://cloud.example/v1/worker/run-attempts/${ATTEMPT_ID}/events`, {
          method: "POST",
          headers: {
            authorization: "Bearer attempt-token",
            "content-type": "application/json",
          },
          body: JSON.stringify({
            runner_instance_id: "token=raw-worker-runner-body-secret",
            event_type: "worker.debug",
          }),
        }),
        url: new URL(`https://cloud.example/v1/worker/run-attempts/${ATTEMPT_ID}/events`),
        code: "invalid_request",
        message: "/runner_instance_id must be a UUID",
        forbidden: "raw-worker-runner-body-secret",
      },
      {
        request: new Request("https://cloud.example/v1/worker/run-attempts/%E0%A4%A-token=raw-attempt-path-secret/fail", {
          method: "POST",
          headers: {
            authorization: "Bearer attempt-token",
            "content-type": "application/json",
          },
          body: JSON.stringify({
            runner_instance_id: RUNNER_INSTANCE_ID,
            message: "worker failed",
          }),
        }),
        url: new URL("https://cloud.example/v1/worker/run-attempts/%E0%A4%A-token=raw-attempt-path-secret/fail"),
        code: "invalid_path_param",
        message: "/attempt_id must be valid percent-encoded UTF-8",
        forbidden: "raw-attempt-path-secret",
      },
      {
        request: new Request(`https://cloud.example/v1/packages/${digest}/content`, {
          headers: {
            authorization: "Bearer attempt-token",
            "x-bucephalus-attempt-id": "token=raw-attempt-header-secret",
          },
        }),
        url: new URL(`https://cloud.example/v1/packages/${digest}/content`),
        code: "invalid_request",
        message: "/attempt_id must be a UUID",
        forbidden: "raw-attempt-header-secret",
      },
    ];

    for (const testCase of cases) {
      let caught: unknown;
      try {
        await handleRunRoute(
          testCase.request,
          testCase.url,
          packages as unknown as PackageRepository,
          runs as unknown as RunRepository,
          {} as RuntimeRepository,
          "worker-token",
          authContext("user-a"),
        );
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(HttpError);
      const error = caught as HttpError;
      expect(error.status).toBe(400);
      expect(error.code).toBe(testCase.code);
      expect(error.message).toBe(testCase.message);
      const encoded = JSON.stringify({ message: error.message, detail: error.detail });
      expect(encoded).not.toContain(testCase.forbidden);
      expect(encoded).not.toContain("/Users/alice");
    }
    expect(calls).toEqual([]);
  });

  test("keeps env values and secret refs in worker claim responses", async () => {
    const runs = {
      async claimNextRun() {
        return {
          run: runRecord(),
          attempt: attemptRecord("attempt-token"),
        };
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/worker/runs/claim", {
        method: "POST",
        headers: {
          authorization: "Bearer worker-token",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          runner_instance_id: RUNNER_INSTANCE_ID,
        }),
      }),
      new URL("https://cloud.example/v1/worker/runs/claim"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    expect(body.attempt.attempt_token).toBe("attempt-token");
    expect(body.run.env.SENSITIVE_ENV).toBe("secret-env-value");
    expect(body.run.secret_refs.OPENAI_API_KEY).toBe("gcp-secret-manager://projects/acme/secrets/openai/versions/1");
  });

  test("worker attempt heartbeat requires the attempt token", async () => {
    const observed: { token?: string; attemptId?: string; runnerInstanceId?: string | null | undefined } = {};
    const runs = {
      async verifyAttemptToken(input: { token: string; attemptId: string; runnerInstanceId?: string | null }) {
        observed.token = input.token;
        observed.attemptId = input.attemptId;
        observed.runnerInstanceId = input.runnerInstanceId;
      },
      async heartbeatAttempt() {
        return attemptRecord();
      },
    };

    const response = await handleRunRoute(
      new Request(`https://cloud.example/v1/worker/run-attempts/${ATTEMPT_ID}/heartbeat`, {
        method: "POST",
        headers: {
          authorization: "Bearer attempt-token",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          runner_instance_id: RUNNER_INSTANCE_ID,
        }),
      }),
      new URL(`https://cloud.example/v1/worker/run-attempts/${ATTEMPT_ID}/heartbeat`),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
    );

    expect(response).not.toBeNull();
    expect(observed).toEqual({
      token: "attempt-token",
      attemptId: ATTEMPT_ID,
      runnerInstanceId: RUNNER_INSTANCE_ID,
    });
  });

  test("worker event responses redact public-boundary payload fields", async () => {
    const runs = {
      async verifyAttemptToken() {},
      async appendRunEvent(): Promise<RunEventRecord> {
        return eventRecord({
          payload: {
            message: "failed in /Users/alice/private/work token=raw-event-token",
            log_url: "file:///private/tmp/bucephalus/event.log",
            nested: {
              api_key: "sk-abcdefghijklmnopqrstuvwxyz",
            },
          },
        });
      },
    };

    const response = await handleRunRoute(
      new Request(`https://cloud.example/v1/worker/run-attempts/${ATTEMPT_ID}/events`, {
        method: "POST",
        headers: {
          authorization: "Bearer attempt-token",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          runner_instance_id: RUNNER_INSTANCE_ID,
          event_type: "worker.debug",
          payload: {
            message: "raw request payload",
          },
        }),
      }),
      new URL(`https://cloud.example/v1/worker/run-attempts/${ATTEMPT_ID}/events`),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    const encoded = JSON.stringify(body);
    expect(body.event.payload.message).toContain("[redacted-local-path]");
    expect(body.event.payload.message).toContain("token=[redacted-secret]");
    expect(body.event.payload.log_url).toBe("file://[redacted-local-path]");
    expect(body.event.payload.nested.api_key).toBe("[redacted]");
    expect(encoded).not.toContain("/Users/alice");
    expect(encoded).not.toContain("/private/tmp");
    expect(encoded).not.toContain("raw-event-token");
    expect(encoded).not.toContain("sk-abcdefghijklmnopqrstuvwxyz");
  });

  test("worker event payloads are sanitized before durable storage", async () => {
    const observed: { eventType?: string; payload?: JsonObject } = {};
    const runs = {
      async verifyAttemptToken() {},
      async appendRunEvent(input: { eventType: string; payload: JsonObject }): Promise<RunEventRecord> {
        observed.eventType = input.eventType;
        observed.payload = input.payload;
        return eventRecord({
          event_type: input.eventType,
          payload: input.payload,
        });
      },
    };

    const response = await handleRunRoute(
      new Request(`https://cloud.example/v1/worker/run-attempts/${ATTEMPT_ID}/events`, {
        method: "POST",
        headers: {
          authorization: "Bearer attempt-token",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          runner_instance_id: RUNNER_INSTANCE_ID,
          event_type: "worker.debug token=raw-event-type-token",
          payload: {
            message: "failed in /Users/alice/private/work token=raw-event-token",
            log_url: "file:///private/tmp/bucephalus/event.log",
            nested: {
              api_key: "sk-abcdefghijklmnopqrstuvwxyz",
            },
          },
        }),
      }),
      new URL(`https://cloud.example/v1/worker/run-attempts/${ATTEMPT_ID}/events`),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
    );

    expect(response).not.toBeNull();
    expect(observed.eventType).toContain("token=[redacted-secret]");
    expect(observed.eventType).not.toContain("raw-event-type-token");
    expect(observed.payload?.message).toContain("[redacted-local-path]");
    expect(observed.payload?.message).toContain("token=[redacted-secret]");
    expect(observed.payload?.log_url).toBe("file://[redacted-local-path]");
    expect((observed.payload?.nested as Record<string, unknown>).api_key).toBe("[redacted]");
    const encoded = JSON.stringify({ stored: observed, response: await response!.json() });
    expect(encoded).not.toContain("/Users/alice");
    expect(encoded).not.toContain("/private/tmp");
    expect(encoded).not.toContain("raw-event-token");
    expect(encoded).not.toContain("raw-event-type-token");
    expect(encoded).not.toContain("sk-abcdefghijklmnopqrstuvwxyz");
  });

  test("worker fail stores redacted error messages before durable run state", async () => {
    const observed: { message?: string } = {};
    const runs = {
      async verifyAttemptToken() {},
      async failAttempt(input: { message: string }): Promise<{ run: ReturnType<typeof runRecord>; attempt: RunAttemptRecord }> {
        observed.message = input.message;
        return {
          run: runRecord({
            status: "failed",
            error_message: input.message,
          }),
          attempt: {
            ...attemptRecord(),
            status: "failed",
            error_message: input.message,
          },
        };
      },
    };

    const response = await handleRunRoute(
      new Request(`https://cloud.example/v1/worker/run-attempts/${ATTEMPT_ID}/fail`, {
        method: "POST",
        headers: {
          authorization: "Bearer attempt-token",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          runner_instance_id: RUNNER_INSTANCE_ID,
          message:
            "core failed in /Users/alice/private/run token=raw-fail-token file:///private/tmp/bucephalus/fail.log",
        }),
      }),
      new URL(`https://cloud.example/v1/worker/run-attempts/${ATTEMPT_ID}/fail`),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
    );

    expect(response).not.toBeNull();
    expect(observed.message).toContain("[redacted-local-path]");
    expect(observed.message).toContain("token=[redacted-secret]");
    expect(observed.message).toContain("file://[redacted-local-path]");
    expect(observed.message).not.toContain("/Users/alice");
    expect(observed.message).not.toContain("/private/tmp");
    expect(observed.message).not.toContain("raw-fail-token");
    const encoded = JSON.stringify(await response!.json());
    expect(encoded).not.toContain("/Users/alice");
    expect(encoded).not.toContain("/private/tmp");
    expect(encoded).not.toContain("raw-fail-token");
  });

  test("package content download requires an attempt token for that package", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-package-content-"));
    const previousEnv = captureEnv(["BUCEPHALUS_CLOUD_DATA_DIR"]);
    process.env.BUCEPHALUS_CLOUD_DATA_DIR = root;
    try {
      const packagePath = join(root, "uploads", "upload-1", "content.blob");
      await mkdir(join(root, "uploads", "upload-1"), { recursive: true });
      await writeFile(packagePath, "package bytes");
      const digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
      const observed: { token?: string; attemptId?: string; packageDigest?: string | null | undefined } = {};
      const packages = {
        async getArtifact() {
          return {
            package_digest: digest,
            upload_id: "upload-1",
            storage_path: packagePath,
            byte_size: 13,
            media_type: "application/gzip",
            manifest_json: {},
            resolved_experiment_json: {},
            target: null,
            image_refs: [],
            diagnostics: [],
            status: "accepted",
            created_at: "2026-06-04T00:00:00Z",
            updated_at: "2026-06-04T00:00:00Z",
          };
        },
      };
      const runs = {
        async verifyAttemptToken(input: { token: string; attemptId: string; packageDigest?: string | null }) {
          observed.token = input.token;
          observed.attemptId = input.attemptId;
          observed.packageDigest = input.packageDigest;
        },
      };

      const response = await handleRunRoute(
        new Request(`https://cloud.example/v1/packages/${digest}/content`, {
          headers: {
            authorization: "Bearer attempt-token",
            "x-bucephalus-attempt-id": ATTEMPT_ID,
          },
        }),
        new URL(`https://cloud.example/v1/packages/${digest}/content`),
        packages as unknown as PackageRepository,
        runs as unknown as RunRepository,
        {} as RuntimeRepository,
        "worker-token",
      );

      expect(response).not.toBeNull();
      expect(await response!.text()).toBe("package bytes");
      expect(observed).toEqual({
        token: "attempt-token",
        attemptId: ATTEMPT_ID,
        packageDigest: digest,
      });
    } finally {
      restoreEnv(previousEnv);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("package content download can stream an artifact stored in R2", async () => {
    const digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const previousFetch = globalThis.fetch;
    const previousEnv = captureEnv([
      "BUCEPHALUS_CLOUD_STORAGE_BACKEND",
      "BUCEPHALUS_CLOUD_R2_ACCOUNT_ID",
      "BUCEPHALUS_CLOUD_R2_BUCKET",
      "BUCEPHALUS_CLOUD_R2_ACCESS_KEY_ID",
      "BUCEPHALUS_CLOUD_R2_SECRET_ACCESS_KEY",
    ]);
    const requests: string[] = [];
    globalThis.fetch = (async (url: string | URL | Request) => {
      requests.push(String(url));
      return new Response("package bytes", { status: 200 });
    }) as typeof fetch;
    process.env.BUCEPHALUS_CLOUD_STORAGE_BACKEND = "r2";
    process.env.BUCEPHALUS_CLOUD_R2_ACCOUNT_ID = "account-id";
    process.env.BUCEPHALUS_CLOUD_R2_BUCKET = "buc-artifacts";
    process.env.BUCEPHALUS_CLOUD_R2_ACCESS_KEY_ID = "access-key";
    process.env.BUCEPHALUS_CLOUD_R2_SECRET_ACCESS_KEY = "secret-key";
    try {
      const packages = {
        async getArtifact() {
          return {
            package_digest: digest,
            upload_id: "upload-1",
            storage_path: "r2://buc-artifacts/uploads/upload-1/content.blob",
            byte_size: 13,
            media_type: "application/gzip",
            manifest_json: {},
            resolved_experiment_json: {},
            target: null,
            image_refs: [],
            diagnostics: [],
            status: "accepted",
            created_at: "2026-06-04T00:00:00Z",
            updated_at: "2026-06-04T00:00:00Z",
          };
        },
      };
      const runs = {
        async verifyAttemptToken() {},
      };

      const response = await handleRunRoute(
        new Request(`https://cloud.example/v1/packages/${digest}/content`, {
          headers: {
            authorization: "Bearer attempt-token",
            "x-bucephalus-attempt-id": ATTEMPT_ID,
          },
        }),
        new URL(`https://cloud.example/v1/packages/${digest}/content`),
        packages as unknown as PackageRepository,
        runs as unknown as RunRepository,
        {} as RuntimeRepository,
        "worker-token",
      );

      expect(await response!.text()).toBe("package bytes");
      expect(requests).toEqual([
        "https://account-id.r2.cloudflarestorage.com/buc-artifacts/uploads/upload-1/content.blob",
      ]);
    } finally {
      globalThis.fetch = previousFetch;
      restoreEnv(previousEnv);
    }
  });

  test("package content download treats invalid stored object paths as unavailable", async () => {
    const digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const previousFetch = globalThis.fetch;
    const previousEnv = captureEnv([
      "BUCEPHALUS_CLOUD_STORAGE_BACKEND",
      "BUCEPHALUS_CLOUD_R2_ACCOUNT_ID",
      "BUCEPHALUS_CLOUD_R2_BUCKET",
      "BUCEPHALUS_CLOUD_R2_ACCESS_KEY_ID",
      "BUCEPHALUS_CLOUD_R2_SECRET_ACCESS_KEY",
    ]);
    let fetchCalls = 0;
    globalThis.fetch = (async () => {
      fetchCalls += 1;
      return new Response("should not fetch", { status: 200 });
    }) as typeof fetch;
    process.env.BUCEPHALUS_CLOUD_STORAGE_BACKEND = "r2";
    process.env.BUCEPHALUS_CLOUD_R2_ACCOUNT_ID = "account-id";
    process.env.BUCEPHALUS_CLOUD_R2_BUCKET = "buc-artifacts";
    process.env.BUCEPHALUS_CLOUD_R2_ACCESS_KEY_ID = "access-key";
    process.env.BUCEPHALUS_CLOUD_R2_SECRET_ACCESS_KEY = "secret-key";
    try {
      const packages = {
        async getArtifact() {
          return {
            package_digest: digest,
            upload_id: "upload-1",
            storage_path: "/Users/alice/private/customer-a-prod-openai-token.env",
            byte_size: 13,
            media_type: "application/gzip",
            manifest_json: {},
            resolved_experiment_json: {},
            target: null,
            image_refs: [],
            diagnostics: [],
            status: "accepted",
            created_at: "2026-06-04T00:00:00Z",
            updated_at: "2026-06-04T00:00:00Z",
          };
        },
      };
      const runs = {
        async verifyAttemptToken() {},
      };

      let caught: unknown;
      try {
        await handleRunRoute(
          new Request(`https://cloud.example/v1/packages/${digest}/content`, {
            headers: {
              authorization: "Bearer attempt-token",
              "x-bucephalus-attempt-id": ATTEMPT_ID,
            },
          }),
          new URL(`https://cloud.example/v1/packages/${digest}/content`),
          packages as unknown as PackageRepository,
          runs as unknown as RunRepository,
          {} as RuntimeRepository,
          "worker-token",
        );
      } catch (error) {
        caught = error;
      }

      expect(caught).toBeInstanceOf(HttpError);
      const error = caught as HttpError;
      expect(error.status).toBe(409);
      expect(error.code).toBe("package_content_unavailable");
      expect(error.message).toBe("Package artifact content is unavailable");
      expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain("customer-a");
      expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain("openai-token");
      expect(fetchCalls).toBe(0);
    } finally {
      globalThis.fetch = previousFetch;
      restoreEnv(previousEnv);
    }
  });

  test("lists only runs for the authenticated owner", async () => {
    const observed: { ownerKey?: string | undefined } = {};
    const runs = {
      async listRuns(input: { ownerKey?: string | undefined }) {
        observed.ownerKey = input.ownerKey;
        return [];
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs"),
      new URL("https://cloud.example/v1/runs"),
      {} as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(observed.ownerKey).toBe("issuer:user-a");
  });

  test("creates runs only from packages owned by the authenticated owner", async () => {
    const observed: { packageOwnerKey?: string | undefined; runOwnerKey?: string | null | undefined } = {};
    const packages = {
      async getArtifact(_digest: string, ownerKey?: string) {
        observed.packageOwnerKey = ownerKey;
        return {
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          upload_id: "upload-1",
          storage_path: "/tmp/package.tgz",
          byte_size: 1,
          media_type: "application/gzip",
          manifest_json: {
            schema_version: "sealed_run_package_v2",
            resolved_experiment: {
              runtime: {
                compute: { backend: "local-docker" },
              },
            },
          },
          resolved_experiment_json: {},
          target: null,
          image_refs: [],
          diagnostics: [],
          status: "accepted",
          created_at: "2026-06-04T00:00:00Z",
          updated_at: "2026-06-04T00:00:00Z",
        };
      },
    };
    const runs = {
      async createRun(input: { ownerKey?: string | null | undefined }) {
        observed.runOwnerKey = input.ownerKey;
        return runRecord();
      },
    };

    await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-b"),
    );

    expect(observed.packageOwnerKey).toBe("issuer:user-b");
    expect(observed.runOwnerKey).toBe("issuer:user-b");
  });

  test("package responses expose declared secret requirements without values", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          manifest_json: {
            schema_version: "sealed_run_package_v2",
            local_path: "/Users/alice/private/package",
            mirror: "https://mirror-user:mirror-secret@mirror.example/package?token=raw-query#frag",
          },
          resolved_experiment_json: {
            runtime: {
              secrets: [
                {
                  name: "OPENAI_API_KEY",
                  from: "file",
                  mount: {
                    target: "/run/secrets/openai",
                  },
                },
              ],
            },
            local_workspace: "/Users/alice/private/workspace",
            token_hint: "raw-package-token",
          },
          image_refs: [
            "https://image-user:image-secret@registry.example/repo?token=raw-image-query#frag",
          ],
          diagnostics: [{
            severity: "warning",
            code: "unsafe_archive_path",
            pointer: "/manifest/local_path",
            message: "manifest referenced /Users/alice/private/package token=raw-diagnostic-token",
          }],
        });
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/packages/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
      new URL("https://cloud.example/v1/packages/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
      packages as unknown as PackageRepository,
      {} as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    const body = await response!.json();
    expect(body.secret_requirements).toEqual([
      {
        id: "OPENAI_API_KEY",
        target: "/run/secrets/openai",
        required_for_variants: [],
      },
    ]);
    expect(body.manifest_json.local_path).toBe("[redacted-local-path]");
    expect(body.manifest_json.mirror).toBe("https://mirror.example/package [redacted URL credentials/query]");
    expect(body.resolved_experiment_json.local_workspace).toBe("[redacted-local-path]");
    expect(body.resolved_experiment_json.token_hint).toBe("[redacted]");
    expect(body.image_refs[0]).toBe("https://registry.example/repo [redacted URL credentials/query]");
    expect(body.diagnostics[0].message).toContain("[redacted-local-path]");
    const text = JSON.stringify(body);
    expect(text).not.toContain("sk-");
    expect(text).not.toContain("/Users/alice");
    expect(text).not.toContain("mirror-user");
    expect(text).not.toContain("mirror-secret");
    expect(text).not.toContain("image-user");
    expect(text).not.toContain("image-secret");
    expect(text).not.toContain("raw-query");
    expect(text).not.toContain("raw-image-query");
    expect(text).not.toContain("raw-package-token");
    expect(text).not.toContain("raw-diagnostic-token");
  });

  test("package responses reject malformed package secret declarations", async () => {
    for (const [secrets, message] of [
      ["OPENAI_API_KEY", "/runtime/secrets must be an array"],
      [[null], "/runtime/secrets/0 must be an object"],
      [[{ from: "env" }], "/runtime/secrets/0/name is required"],
      [[{ name: "OPENAI_API_KEY" }], "/runtime/secrets/0/from is required"],
      [[{ name: "OPENAI_API_KEY", from: "vault" }], "/runtime/secrets/0/from 'vault' is not supported"],
      [[{ name: "OPENAI=API", from: "env" }], "/runtime/secrets/0/name must not contain '='"],
      [[{ name: "OPENAI API KEY", from: "env" }], "/runtime/secrets/0/name must be a Cloud secret id"],
      [[{ name: "OPENAI_API_KEY", from: "file", mount: "/run/secrets/openai" }], "/runtime/secrets/0/mount must be an object"],
      [[{ name: "OPENAI_API_KEY", from: "file", mount: { target: 7 } }], "/runtime/secrets/0/mount/target must be a string"],
      [[{ name: "OPENAI_API_KEY", from: "file", mount: { target: "run/secrets/openai" } }], "/runtime/secrets/0/mount/target must be an absolute container path"],
      [[{ name: "OPENAI_API_KEY", from: "file", mount: { target: "/run/../secrets/openai" } }], "/runtime/secrets/0/mount/target must be an absolute container path"],
      [[{ name: "OPENAI_API_KEY", from: "file", mount: { target: "/run/secrets/openai\nleak" } }], "/runtime/secrets/0/mount/target must be an absolute container path"],
      [[{ name: "OPENAI_API_KEY", from: "file", required_for_variants: "codex" }], "/runtime/secrets/0/required_for_variants must be an array"],
      [[{ name: "OPENAI_API_KEY", from: "file", required_for_variants: ["codex", ""] }], "/runtime/secrets/0/required_for_variants/1 must be a non-empty string"],
      [[{ name: "OPENAI_API_KEY", from: "file", mount: { required_for_variants: [null] } }], "/runtime/secrets/0/mount/required_for_variants/0 must be a non-empty string"],
      [[
        { name: "OPENAI_API_KEY", from: "env" },
        { name: "OPENAI_API_KEY", from: "file", mount: { target: "/run/secrets/openai" } },
      ], "Duplicate runtime secret declaration 'OPENAI_API_KEY'"],
    ] as const) {
      const packages = {
        async getArtifact() {
          return packageRecordWithSecrets({
            resolved_experiment_json: {
              runtime: {
                secrets,
              },
            },
          });
        },
      };

      await expect(handleRunRoute(
        new Request("https://cloud.example/v1/packages/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        new URL("https://cloud.example/v1/packages/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        packages as unknown as PackageRepository,
        {} as RunRepository,
        {} as RuntimeRepository,
        "worker-token",
        authContext("user-a"),
      )).rejects.toThrow(message);
    }
  });

  test("package responses reject legacy agent secret file aliases instead of underreporting requirements", async () => {
    for (const [patch, message] of [
      [
        {
          trial_runtime: {
            agent: {
              secret_files: [{ name: "OPENAI_API_KEY", target: "/run/secrets/openai" }],
            },
          },
        },
        "/trial_runtime/agent/secret_files is not supported",
      ],
      [
        {
          agent: {
            secret_files: [{ name: "OPENAI_API_KEY", target: "/run/secrets/openai" }],
          },
        },
        "/agent/secret_files is not supported",
      ],
      [
        {
          stages: {
            agent: {
              secret_files: [{ name: "OPENAI_API_KEY", target: "/run/secrets/openai" }],
            },
          },
        },
        "/stages/agent/secret_files is not supported",
      ],
    ] as const) {
      const packages = {
        async getArtifact() {
          return packageRecordWithSecrets({
            resolved_experiment_json: {
              runtime: {
                compute: { backend: "local-docker" },
                secrets: [],
              },
              ...patch,
            },
          });
        },
      };

      await expect(handleRunRoute(
        new Request("https://cloud.example/v1/packages/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        new URL("https://cloud.example/v1/packages/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        packages as unknown as PackageRepository,
        {} as RunRepository,
        {} as RuntimeRepository,
        "worker-token",
        authContext("user-a"),
      )).rejects.toThrow(message);
    }
  });

  test("package responses expose sorted package secret variant requirements", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          resolved_experiment_json: {
            runtime: {
              secrets: [
                {
                  name: "CODEX_OAUTH",
                  from: "file",
                  mount: {
                    target: "/run/secrets/codex",
                    required_for_variants: ["agent-b", "agent-a", "agent-b"],
                  },
                },
                {
                  name: "OPENAI_API_KEY",
                  from: "env",
                  required_for_variants: ["agent-a"],
                },
              ],
            },
          },
        });
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/packages/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
      new URL("https://cloud.example/v1/packages/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
      packages as unknown as PackageRepository,
      {} as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect((await response!.json()).secret_requirements).toEqual([
      {
        id: "CODEX_OAUTH",
        target: "/run/secrets/codex",
        required_for_variants: ["agent-a", "agent-b"],
      },
      {
        id: "OPENAI_API_KEY",
        target: "",
        required_for_variants: ["agent-a"],
      },
    ]);
  });

  test("run creation rejects missing package secret refs before queueing", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets();
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("Run secret refs must match");
  });

  test("run creation rejects accepted packages whose content is unavailable", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          storage_path: null,
        });
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          secret_refs: {
            OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("Package artifact content is unavailable");
  });

  test("run creation rejects invalid Core launch runtime options before queueing", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          resolved_experiment_json: {
            runtime: {
              compute: { backend: "local-docker" },
              secrets: [],
            },
          },
        });
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    for (const [runtime_options, message] of [
      [null, "/runtime_options must be a JSON object"],
      [["materialize=full"], "/runtime_options must be a JSON object"],
      [{ materialize: "metadata" }, "Unsupported Core materialize mode"],
      [{ smoke_test: "true" }, "/runtime_options/smoke_test must be a boolean"],
    ] as const) {
      await expect(handleRunRoute(
        new Request("https://cloud.example/v1/runs", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            runtime_options,
          }),
        }),
        new URL("https://cloud.example/v1/runs"),
        packages as unknown as PackageRepository,
        runs as unknown as RunRepository,
        {} as RuntimeRepository,
        "worker-token",
        authContext("user-a"),
      )).rejects.toThrow(message);
    }
  });

  test("run creation rejects malformed env and secret ref maps before queueing", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          resolved_experiment_json: {
            runtime: {
              secrets: [],
            },
          },
        });
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    for (const [bodyPatch, message] of [
      [{ env: ["MODEL=gpt-4.1"] }, "/env must be an object"],
      [{ env: null }, "/env must be an object"],
      [{ env: { MODEL: 4 } }, "/env/MODEL must be a string"],
      [{ env: { "bad-key": "value" } }, "Invalid Cloud run env key 'bad-key'"],
      [{ secret_refs: "OPENAI_API_KEY=ref" }, "/secret_refs must be an object"],
      [{ secret_refs: [] }, "/secret_refs must be an object"],
      [{ secret_refs: { OPENAI_API_KEY: 7 } }, "/secret_refs/OPENAI_API_KEY must be a string"],
    ] as const) {
      await expect(handleRunRoute(
        new Request("https://cloud.example/v1/runs", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ...bodyPatch,
          }),
        }),
        new URL("https://cloud.example/v1/runs"),
        packages as unknown as PackageRepository,
        runs as unknown as RunRepository,
        {} as RuntimeRepository,
        "worker-token",
        authContext("user-a"),
      )).rejects.toThrow(message);
    }
  });

  test("run creation rejects masked package-authored Cloud requirement failures before queueing", async () => {
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    for (const [resolved_experiment_json, runtime_options, message] of [
      [
        {
          runtime: {
            compute: { backend: "kubernetes" },
            secrets: [],
          },
        },
        { backend: "runner-docker" },
        "Unsupported Cloud run backend 'kubernetes'",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker", memory_mb: 0 },
            secrets: [],
          },
        },
        { memory_mb: 4096 },
        "/runtime/compute/memory_mb must be a positive integer",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker", cpu_count: 2147483648 },
            secrets: [],
          },
        },
        { cpu_count: 4 },
        "/runtime/compute/cpu_count must be a positive integer",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            secrets: [],
          },
          execution: {
            agent_site: "host",
          },
        },
        {
          trial_runtime: {
            execution: {
              agent_site: "agent_container",
            },
          },
        },
        "agent_site=host",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            secrets: [],
          },
          stages: {
            agent: {
              secret_files: [{ name: "OPENAI_API_KEY", target: "/run/secrets/openai" }],
            },
          },
        },
        {},
        "/stages/agent/secret_files is not supported",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            secrets: [],
          },
          trial_runtime: {
            grader: {
              strategy: "host",
              command: ["__BUCEPHALUS_HOST_GRADER_CAPABILITY__/grader/run.sh"],
              host: {
                capability: "grader-capability",
              },
            },
          },
        },
        {
          trial_runtime: {
            grader: {
              strategy: "separate",
            },
          },
        },
        "grader.strategy=host",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker", accelerators: "nvidia-l4" },
            secrets: [],
          },
        },
        {
          accelerators: ["nvidia-l4"],
        },
        "/runtime/compute/accelerators must be an array of strings",
      ],
      [
        {
          runtime: {
            compute: { backend: "modal", accelerators: ["nvidia-l4"] },
            secrets: [],
          },
        },
        {},
        "modal Cloud runs do not support accelerator requirements",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            secrets: [],
          },
        },
        {
          backend: "modal",
          accelerators: ["nvidia-l4"],
        },
        "modal Cloud runs do not support accelerator requirements",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            secrets: [],
          },
          trial_runtime: {
            agent: {
              image: 7,
            },
          },
        },
        {},
        "/trial_runtime/agent/image must be a non-empty image ref string",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            secrets: [],
          },
        },
        {
          trial_runtime: {
            agent: {
              sidecars: ["cache"],
            },
          },
        },
        "declare sidecars in the package YAML or use runtime_options.sidecars",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            secrets: [],
          },
        },
        {
          trial_runtime: {
            agent: {
              ephemerals: ["cache"],
            },
          },
        },
        "declare sidecars in the package YAML or use runtime_options.sidecars",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            secrets: [],
          },
        },
        {
          trial_runtime: {
            grader: {
              ephemerals: ["cache"],
            },
          },
        },
        "declare sidecars in the package YAML or use runtime_options.sidecars",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            secrets: [],
          },
        },
        {
          trial_runtime: {
            grader: {
              sidecars: ["cache"],
            },
          },
        },
        "declare sidecars in the package YAML or use runtime_options.sidecars",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            secrets: [],
          },
        },
        {
          trial_runtime: {
            agent: {
              image: "ghcr.io/acme/agent:latest",
            },
          },
        },
        "digest-pinned remote registry refs",
      ],
      [
        {
          runtime: {
            compute: { backend: "modal" },
            network: {
              egress: ["api.openai.com"],
            },
            secrets: [],
          },
        },
        {
          backend: "runner-docker",
        },
        "modal Cloud runs do not support network perimeter",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            secrets: [],
          },
        },
        {
          backend: "modal",
          network: {
            default: "allowlist_enforced",
            egress: ["api.openai.com"],
          },
        },
        "modal Cloud runs do not support network perimeter",
      ],
    ] as const) {
      const packages = {
        async getArtifact() {
          return packageRecordWithSecrets({ resolved_experiment_json });
        },
      };

      await expect(handleRunRoute(
        new Request("https://cloud.example/v1/runs", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            runtime_options,
          }),
        }),
        new URL("https://cloud.example/v1/runs"),
        packages as unknown as PackageRepository,
        runs as unknown as RunRepository,
        {} as RuntimeRepository,
        "worker-token",
        authContext("user-a"),
      )).rejects.toThrow(message);
    }
  });

  test("run creation rejects secret-looking plain env keys before queueing without echoing values", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          resolved_experiment_json: {
            runtime: {
              secrets: [],
            },
          },
        });
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    let message = "";
    try {
      await handleRunRoute(
        new Request("https://cloud.example/v1/runs", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            env: {
              MODEL: "gpt-4.1",
              OPENAI_API_KEY: "sk-live-secret",
            },
          }),
        }),
        new URL("https://cloud.example/v1/runs"),
        packages as unknown as PackageRepository,
        runs as unknown as RunRepository,
        {} as RuntimeRepository,
        "worker-token",
        authContext("user-a"),
      );
    } catch (err) {
      message = err instanceof Error ? err.message : String(err);
    }

    expect(message).toContain("OPENAI_API_KEY");
    expect(message).toContain("secret_refs");
    expect(message).toContain("allow_secret_env");
    expect(message).not.toContain("sk-live-secret");
  });

  test("run creation allows explicit secret env override and persists non-secret env", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          resolved_experiment_json: {
            runtime: {
              secrets: [],
            },
          },
        });
      },
    };
    const observed: { env?: Record<string, string> } = {};
    const runs = {
      async createRun(input: { env: Record<string, string> }) {
        observed.env = input.env;
        return runRecord();
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          env: {
            MODEL: "gpt-4.1",
            OPENAI_API_KEY: "intentionally-public-test-fixture",
          },
          allow_secret_env: true,
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    expect(observed.env).toEqual({
      MODEL: "gpt-4.1",
      OPENAI_API_KEY: "intentionally-public-test-fixture",
    });
  });

  test("run creation persists package-authored Cloud requirements before queueing", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          resolved_experiment_json: {
            runtime: {
              compute: {
                backend: "local-docker",
                config: { max_parallel: 2 },
              },
              network: {
                default: "allowlist_enforced",
                egress: ["API.OpenAI.com", "storage.googleapis.com"],
              },
              secrets: [
                {
                  name: "OPENAI_API_KEY",
                  from: "env",
                },
              ],
            },
            scheduling: {
              max_concurrency: 3,
            },
            policy: {
              timeout_ms: 450000,
              task_sandbox: {
                resources: {
                  cpu_count: 8,
                  memory_mb: 32768,
                },
              },
            },
            sidecars: {
              cache: {
                image: "ghcr.io/acme/cache@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                lifecycle: "per-trial",
              },
            },
            trial_runtime: {
              agent: {
                sidecars: ["cache"],
              },
            },
          },
        });
      },
    };
    const observed: { runRequirements?: Record<string, unknown> } = {};
    const runs = {
      async createRun(input: { runRequirements: Record<string, unknown> }) {
        observed.runRequirements = input.runRequirements;
        return runRecord();
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          secret_refs: {
            OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    expect(observed.runRequirements).toMatchObject({
      requires: [
        "core_runner",
        "docker_daemon",
        "registry_pull",
        "secret_resolver",
        "network_perimeter",
        "sidecar:cache",
      ],
      secret_ids: ["OPENAI_API_KEY"],
      network_perimeter: {
        default: "allowlist_enforced",
        task_sandbox: "allowlist_enforced",
        agent: "allowlist_enforced",
        egress_hosts: ["api.openai.com", "storage.googleapis.com"],
      },
      sidecars: ["cache"],
      cpu_count: 8,
      memory_mb: 32768,
      timeout_ms: 450000,
      max_parallel_trials: 3,
    });
  });

  test("run creation persists modern package ephemeral requirements before queueing", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          resolved_experiment_json: {
            runtime: {
              compute: {
                backend: "local-docker",
              },
              secrets: [],
            },
            ephemerals: {
              "mcp-bash": {
                image: "ghcr.io/acme/mcp-bash@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                lifecycle: "per-trial",
              },
            },
            stages: {
              agent: {
                ephemerals: ["mcp-bash"],
              },
            },
          },
        });
      },
    };
    const observed: { runRequirements?: Record<string, unknown> } = {};
    const runs = {
      async createRun(input: { runRequirements: Record<string, unknown> }) {
        observed.runRequirements = input.runRequirements;
        return runRecord();
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    expect(observed.runRequirements).toMatchObject({
      requires: [
        "core_runner",
        "docker_daemon",
        "registry_pull",
        "sidecar:mcp-bash",
      ],
      sidecars: ["mcp-bash"],
      image_refs: [
        "ghcr.io/acme/mcp-bash@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      ],
    });
  });

  test("run creation persists runtime option image requirements before queueing", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          resolved_experiment_json: {
            runtime: {
              compute: {
                backend: "local-docker",
              },
              secrets: [],
            },
          },
        });
      },
    };
    const observed: { runRequirements?: Record<string, unknown> } = {};
    const runs = {
      async createRun(input: { runRequirements: Record<string, unknown> }) {
        observed.runRequirements = input.runRequirements;
        return runRecord();
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          runtime_options: {
            trial_runtime: {
              agent: {
                image: "ghcr.io/acme/agent@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
              },
            },
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    expect(observed.runRequirements?.image_refs).toEqual([
      "ghcr.io/acme/agent@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    ]);
  });

  test("run creation persists package and runtime accelerator requirements before queueing", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          resolved_experiment_json: {
            runtime: {
              compute: {
                backend: "local-docker",
                accelerators: ["nvidia-l4", "tpu-v5e"],
              },
              secrets: [],
            },
            policy: {
              task_sandbox: {
                resources: {
                  accelerators: ["amd-mi300", "nvidia-l4"],
                },
              },
            },
          },
        });
      },
    };
    const observed: { runRequirements?: Record<string, unknown> } = {};
    const runs = {
      async createRun(input: { runRequirements: Record<string, unknown> }) {
        observed.runRequirements = input.runRequirements;
        return runRecord();
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          runtime_options: {
            accelerators: ["nvidia-a100", "tpu-v5e"],
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    expect(observed.runRequirements).toMatchObject({
      requires: [
        "core_runner",
        "docker_daemon",
        "registry_pull",
        "accelerator:amd-mi300",
        "accelerator:nvidia-a100",
        "accelerator:nvidia-l4",
        "accelerator:tpu-v5e",
      ],
      accelerators: ["amd-mi300", "nvidia-a100", "nvidia-l4", "tpu-v5e"],
    });
  });

  test("run creation merges runtime Cloud network overrides with package-authored network blocks", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          resolved_experiment_json: {
            runtime: {
              compute: {
                backend: "local-docker",
              },
              network: {
                task_sandbox: "none",
                egress: ["storage.googleapis.com"],
              },
              secrets: [],
            },
          },
        });
      },
    };
    const observed: { runRequirements?: Record<string, unknown> } = {};
    const runs = {
      async createRun(input: { runRequirements: Record<string, unknown> }) {
        observed.runRequirements = input.runRequirements;
        return runRecord();
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          runtime_options: {
            network: {
              default: "allowlist_enforced",
              egress: ["api.openai.com", "storage.googleapis.com"],
            },
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    expect(observed.runRequirements).toMatchObject({
      requires: [
        "core_runner",
        "docker_daemon",
        "registry_pull",
        "network_perimeter",
      ],
      network_perimeter: {
        default: "allowlist_enforced",
        task_sandbox: "allowlist_enforced",
        agent: "allowlist_enforced",
        egress_hosts: ["api.openai.com", "storage.googleapis.com"],
      },
    });
  });

  test("run creation persists egress-only Cloud network requirements before queueing", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          resolved_experiment_json: {
            runtime: {
              compute: {
                backend: "local-docker",
              },
              network: {
                egress: ["storage.googleapis.com"],
              },
              secrets: [],
            },
          },
        });
      },
    };
    const observed: { runRequirements?: Record<string, unknown> } = {};
    const runs = {
      async createRun(input: { runRequirements: Record<string, unknown> }) {
        observed.runRequirements = input.runRequirements;
        return runRecord();
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          runtime_options: {
            network: {
              default: "none",
              egress: ["api.openai.com", "storage.googleapis.com"],
            },
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    expect(observed.runRequirements).toMatchObject({
      requires: [
        "core_runner",
        "docker_daemon",
        "registry_pull",
        "network_perimeter",
      ],
      network_perimeter: {
        default: "none",
        task_sandbox: "none",
        agent: "none",
        egress_hosts: ["api.openai.com", "storage.googleapis.com"],
      },
    });
  });

  test("run creation persists package external API network requirements before queueing", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          resolved_experiment_json: {
            runtime: {
              compute: {
                backend: "local-docker",
              },
              externals: {
                apis: ["storage.googleapis.com"],
              },
              secrets: [],
            },
            externals: {
              apis: ["storage.googleapis.com"],
            },
          },
        });
      },
    };
    const observed: { runRequirements?: Record<string, unknown> } = {};
    const runs = {
      async createRun(input: { runRequirements: Record<string, unknown> }) {
        observed.runRequirements = input.runRequirements;
        return runRecord();
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          runtime_options: {
            network: {
              default: "none",
              egress: ["api.openai.com"],
            },
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    expect(observed.runRequirements).toMatchObject({
      requires: [
        "core_runner",
        "docker_daemon",
        "registry_pull",
        "network_perimeter",
      ],
      network_perimeter: {
        default: "none",
        task_sandbox: "none",
        agent: "none",
        egress_hosts: ["api.openai.com", "storage.googleapis.com"],
      },
    });
  });

  test("run creation rejects undeclared secret refs before queueing", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets();
      },
    };
    const runs = {
      async createRun() {
        throw new Error("createRun should not be called");
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          secret_refs: {
            OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
            EXTRA_TOKEN: "gcp-secret-manager://projects/acme/secrets/extra/versions/latest",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("Run secret refs must match");
  });

  test("run creation accepts env-only package secret refs", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithEnvSecret();
      },
    };
    const observed: { secretRefs?: Record<string, string>; secretIds?: string[] } = {};
    const runs = {
      async createRun(input: { secretRefs: Record<string, string>; runRequirements: { secret_ids: string[] } }) {
        observed.secretRefs = input.secretRefs;
        observed.secretIds = input.runRequirements.secret_ids;
        return runRecord();
      },
    };

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          secret_refs: {
            OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    expect(observed.secretRefs).toEqual({
      OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
    });
    expect(observed.secretIds).toEqual(["OPENAI_API_KEY"]);
  });

  test("run creation requires file secrets even without mount targets", async () => {
    const packages = {
      async getArtifact() {
        return packageRecordWithSecrets({
          resolved_experiment_json: {
            runtime: {
              secrets: [
                {
                  name: "CODEX_OAUTH",
                  from: "file",
                },
              ],
            },
          },
        });
      },
    };
    const observed: { secretRefs?: Record<string, string>; secretIds?: string[] } = {};
    const runs = {
      async createRun(input: { secretRefs: Record<string, string>; runRequirements: { secret_ids: string[] } }) {
        observed.secretRefs = input.secretRefs;
        observed.secretIds = input.runRequirements.secret_ids;
        return runRecord();
      },
    };

    await expect(handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    )).rejects.toThrow("Run secret refs must match");

    const response = await handleRunRoute(
      new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          secret_refs: {
            CODEX_OAUTH: "gcp-secret-manager://projects/acme/secrets/codex-oauth/versions/latest",
          },
        }),
      }),
      new URL("https://cloud.example/v1/runs"),
      packages as unknown as PackageRepository,
      runs as unknown as RunRepository,
      {} as RuntimeRepository,
      "worker-token",
      authContext("user-a"),
    );

    expect(response).not.toBeNull();
    expect(response!.status).toBe(201);
    expect(observed.secretRefs).toEqual({
      CODEX_OAUTH: "gcp-secret-manager://projects/acme/secrets/codex-oauth/versions/latest",
    });
    expect(observed.secretIds).toEqual(["CODEX_OAUTH"]);
  });
});

function runRecord(overrides: Record<string, unknown> = {}) {
  const defaultRunRequirements = {
    executor: "runner-docker",
    requires: [],
    image_refs: [],
    secret_ids: ["OPENAI_API_KEY"],
    network_perimeter: {
      default: "none",
      task_sandbox: "none",
      agent: "none",
      egress_hosts: [],
    },
    sidecars: [],
    accelerators: [],
    arch: "x86_64",
    cpu_count: 1,
    memory_mb: 1024,
    disk_mb: 20480,
    isolation: "reusable_vm",
    timeout_ms: null,
    max_parallel_trials: 1,
  };
  const runRequirementsOverride = isObject(overrides.run_requirements)
    ? overrides.run_requirements
    : {};
  const networkPerimeterOverride = isObject(runRequirementsOverride.network_perimeter)
    ? runRequirementsOverride.network_perimeter
    : {};
  const { run_requirements: _runRequirements, ...restOverrides } = overrides;
  return {
    run_id: RUN_ID,
    package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    run_label: "security",
    status: "created",
    env: {
      PUBLIC_FLAG: "1",
      SENSITIVE_ENV: "secret-env-value",
    },
    secret_refs: {
      OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/1",
    },
    runtime_options: {},
    run_requirements: {
      ...defaultRunRequirements,
      ...runRequirementsOverride,
      network_perimeter: {
        ...defaultRunRequirements.network_perimeter,
        ...networkPerimeterOverride,
      },
    },
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:00Z",
    started_at: null,
    completed_at: null,
    error_message: null,
    ...restOverrides,
  };
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function attemptRecord(attemptToken?: string): RunAttemptRecord {
  return {
    attempt_id: ATTEMPT_ID,
    run_id: RUN_ID,
    worker_id: RUNNER_INSTANCE_ID,
    runner_instance_id: RUNNER_INSTANCE_ID,
    status: "running",
    lease_expires_at: "2026-06-04T00:01:00Z",
    heartbeat_at: "2026-06-04T00:00:00Z",
    started_at: "2026-06-04T00:00:00Z",
    ended_at: null,
    error_message: null,
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:00Z",
    ...(attemptToken ? { attempt_token: attemptToken } : {}),
  };
}

function eventRecord(overrides: Partial<RunEventRecord> = {}): RunEventRecord {
  return {
    event_id: "event-1",
    run_id: RUN_ID,
    attempt_id: ATTEMPT_ID,
    seq: 1,
    event_type: "worker.debug",
    payload: {},
    created_at: "2026-06-04T00:00:00Z",
    ...overrides,
  };
}

function packageRecordWithSecrets(overrides: Record<string, unknown> = {}) {
  return {
    package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    upload_id: "upload-1",
    storage_path: "/tmp/package.tgz",
    byte_size: 1,
    media_type: "application/gzip",
    manifest_json: {},
    resolved_experiment_json: {
      runtime: {
        secrets: [
          {
            name: "OPENAI_API_KEY",
            from: "file",
            mount: {
              target: "/run/secrets/openai",
            },
          },
        ],
      },
    },
    target: null,
    image_refs: [],
    diagnostics: [],
    status: "accepted",
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:00Z",
    ...overrides,
  };
}

function packageRecordWithEnvSecret() {
  return {
    package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    upload_id: "upload-1",
    storage_path: "/tmp/package.tgz",
    byte_size: 1,
    media_type: "application/gzip",
    manifest_json: {},
    resolved_experiment_json: {
      runtime: {
        secrets: [
          {
            name: "OPENAI_API_KEY",
            from: "env",
          },
        ],
      },
    },
    target: null,
    image_refs: [],
    diagnostics: [],
    status: "accepted",
    created_at: "2026-06-04T00:00:00Z",
    updated_at: "2026-06-04T00:00:00Z",
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

function captureEnv(keys: string[]): Record<string, string | undefined> {
  return Object.fromEntries(keys.map((key) => [key, process.env[key]]));
}

function restoreEnv(previous: Record<string, string | undefined>): void {
  for (const [key, value] of Object.entries(previous)) {
    if (value === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }
}
