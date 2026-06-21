import { mkdtemp, mkdir, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { spawn } from "node:child_process";
import { describe, expect, test } from "bun:test";
import * as tar from "tar";
import {
  applyRuntimeNetworkPolicy,
  applyRuntimeNetworkPolicyWithAudit,
  collectRuntimeArtifactUploads,
  collectRuntimeSnapshot,
  coreProgressCompleted,
  coreRunnerEnv,
  coreRunnerFailureMessage,
  demuxDockerExecOutput,
  dockerRegistryAuthHeaders,
  loadWorkerConfig,
  materializeAttemptSecrets,
  materializePackage,
  prePullRunImagesWithAudit,
  processPortForwardRequests,
  processExecRequests,
  resolveRuntimeExecCommandTarget,
  validateAcceleratorRequirementsWithAudit,
  validateSidecarRequirementsWithAudit,
} from "../src/worker";
import { discoverCoreRunIdsFromRunRoot } from "../src/workerEvidence";
import { canonicalJsonStringify, sha256Digest, type JsonObject } from "../src/primitives";

describe("worker lifecycle cleanup helpers", () => {
  test("detects Core schedule completion progress lines exactly", () => {
    expect(coreProgressCompleted("[run] run_20260614_043632_591762_000001: progress 18/18 (100.0%) slot=17 trial=trial_18 status=completed\n")).toBe(true);
    expect(coreProgressCompleted("[run] run_20260614_043632_591762_000001: progress 17/18 (94.4%) slot=16 trial=trial_17 status=completed\n")).toBe(false);
    expect(coreProgressCompleted("[run] run_20260614_043632_591762_000001: progress 18/18 (100.0%) slot=17 trial=trial_18 status=failed\n")).toBe(false);
  });

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

  test("runtime Docker exec target resolution uses trial runtime state", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-exec-target-"));
    try {
      const runRoot = join(root, "run-root");
      const coreRunId = "run_20260529_000001_000001_000001";
      const trialDir = join(runRoot, coreRunId, "trials", "trial:1", "runner");
      await mkdir(trialDir, { recursive: true });
      await writeFile(join(trialDir, "trial_runtime_state.json"), JSON.stringify({
        schema_version: "trial_runtime_state_v1",
        updated_at: "2026-06-18T00:00:00.000Z",
        state: {
          key: { schedule_idx: 3, attempt: 0 },
          slot: { variant_id: "base", task_id: "task-1", repl_idx: 0 },
          phase: "agent_running",
          fs: { attempt_dir: "/tmp/trial", in_dir: "/tmp/trial/in", out_dir: "/tmp/trial/out", telemetry_mounts: [], logs_dir: "/tmp/trial/logs" },
          task_sandbox: {
            container_id: "abcdef1234567890abcdef1234567890abcdef123456",
            image: "python:3.11-slim",
            workdir: "/workspace",
            materialization: { kind: "copy" },
          },
          grading_sandbox: {
            container_id: "grading-container-1",
            strategy: "separate",
            workdir: "/grader",
          },
          ephemerals: [],
          ephemeral_networks: [],
          cleanup: { containers: [] },
        },
      }));

      await expect(resolveRuntimeExecCommandTarget(
        runRoot,
        "TrialContainer",
        "trial-1.task.abcdef1234567890abcdef1234567890",
      )).resolves.toMatchObject({
        mode: "docker_container",
        coreRunId,
        trialId: "trial:1",
        scheduleIdx: 3,
        role: "task",
        containerId: "abcdef1234567890abcdef1234567890abcdef123456",
        workdir: "/workspace",
      });
      await expect(resolveRuntimeExecCommandTarget(runRoot, "Trial", "trial-1")).resolves.toMatchObject({
        mode: "docker_container",
        role: "task",
        containerId: "abcdef1234567890abcdef1234567890abcdef123456",
      });
      await expect(resolveRuntimeExecCommandTarget(runRoot, "ScheduleSlot", `${coreRunId}.3`)).resolves.toMatchObject({
        mode: "docker_container",
        role: "task",
        containerId: "abcdef1234567890abcdef1234567890abcdef123456",
      });
      await expect(resolveRuntimeExecCommandTarget(runRoot, "RunnerInstance", "runner-1")).resolves.toEqual({
        mode: "worker_process",
        resourceKind: "RunnerInstance",
        resourceName: "runner-1",
      });
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("Docker exec stream demux keeps stdout and stderr separate", () => {
    const stdout = Buffer.from("hello\n");
    const stderr = Buffer.from("warn\n");
    const stdoutHeader = Buffer.alloc(8);
    stdoutHeader[0] = 1;
    stdoutHeader.writeUInt32BE(stdout.byteLength, 4);
    const stderrHeader = Buffer.alloc(8);
    stderrHeader[0] = 2;
    stderrHeader.writeUInt32BE(stderr.byteLength, 4);

    expect(demuxDockerExecOutput(Buffer.concat([stdoutHeader, stdout, stderrHeader, stderr]))).toEqual({
      stdout: "hello\n",
      stderr: "warn\n",
    });
  });

  test("collects Core runtime mirrors without absolute workspace paths", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-"));
    try {
      const runRoot = join(root, "run-root");
      const coreRunId = "run_20260529_000001_000001_000001";
      const runtimeDir = join(runRoot, coreRunId, "runtime");
      const trialDir = join(runRoot, coreRunId, "trials", "trial-1");
      await mkdir(runtimeDir, { recursive: true });
      await mkdir(join(trialDir, "runner"), { recursive: true });
      await mkdir(join(trialDir, "agent"), { recursive: true });
      await mkdir(join(runRoot, coreRunId, "evidence"), { recursive: true });
      await writeFile(join(runtimeDir, "run_control.json"), JSON.stringify({ run_id: coreRunId, status: "completed" }));
      await writeFile(join(runtimeDir, "schedule_progress.json"), JSON.stringify({ committed: 1, total: 1 }));
      await writeFile(join(trialDir, "summary.json"), JSON.stringify({ outcome: "success" }));
      await writeFile(join(trialDir, "runner", "contract_trace.json"), JSON.stringify({
        schema_version: "trial_contract_trace_v1",
        overall_status: "ok",
        stages: {
          agent_execution: { status: "ok" },
        },
      }));
      await writeFile(join(trialDir, "agent", "events.jsonl"), [
        JSON.stringify({ event_type: "step", ts: "2026-05-29T00:00:00Z", message: "started" }),
        JSON.stringify({ type: "finish", timestamp: "2026-05-29T00:00:01Z" }),
        "",
      ].join("\n"));
      await writeFile(join(runRoot, coreRunId, "evidence", "evidence_records.jsonl"), `${JSON.stringify({
        schema_version: "evidence_record_v1",
        ids: {
          trial_id: "trial-1",
        },
        schedule_idx: 0,
        attempt: 0,
        evidence: {
          trial_output_ref: "artifact://sha256/output",
        },
      })}\n`);

      const snapshot = await collectRuntimeSnapshot(runRoot, coreRunId);

      expect(snapshot.core_run_id).toBe(coreRunId);
      expect(snapshot.run_dir_name).toBe(coreRunId);
      expect(snapshot.runtime_values.run_control_v2).toEqual({ run_id: coreRunId, status: "completed" });
      expect(snapshot.runtime_values.schedule_progress_v2).toEqual({ committed: 1, total: 1 });
      expect(snapshot.trial_summaries).toEqual([
        {
          trial_id: "trial-1",
          summary: { outcome: "success" },
          contract_trace: {
            schema_version: "trial_contract_trace_v1",
            overall_status: "ok",
            stages: {
              agent_execution: { status: "ok" },
            },
          },
          trial_events: [
            { event_type: "step", ts: "2026-05-29T00:00:00Z", message: "started" },
            { type: "finish", timestamp: "2026-05-29T00:00:01Z" },
          ],
        },
      ]);
      expect(snapshot.evidence_records).toEqual([
        {
          schema_version: "evidence_record_v1",
          ids: {
            trial_id: "trial-1",
          },
          schedule_idx: 0,
          attempt: 0,
          evidence: {
            trial_output_ref: "artifact://sha256/output",
          },
        },
      ]);
      expect(JSON.stringify(snapshot)).not.toContain(root);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("collects first-class runtime artifact uploads from Core trial directories", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-artifacts-"));
    try {
      const runRoot = join(root, "run-root");
      const coreRunId = "run_20260529_000001_000001_000001";
      const trialDir = join(runRoot, coreRunId, "trials", "trial-1");
      await mkdir(join(trialDir, "agent"), { recursive: true });
      await mkdir(join(trialDir, "runner"), { recursive: true });
      await writeFile(join(trialDir, "summary.json"), JSON.stringify({
        ids: {
          trial_id: "trial-from-summary",
        },
        outcome: "success",
      }));
      await writeFile(join(trialDir, "runner", "contract_trace.json"), JSON.stringify({
        ids: {
          trial_id: "trial-from-contract",
          schedule_idx: 7,
          attempt: 2,
        },
      }));
      await writeFile(join(trialDir, "agent", "result.json"), JSON.stringify({
        final: "Peter Gregory generated answer",
      }));
      await writeFile(join(trialDir, "agent", "stdout.log"), "stdout\n");
      await writeFile(join(trialDir, "agent", "stderr.log"), "stderr\n");
      await writeFile(join(trialDir, "agent", "events.jsonl"), `${JSON.stringify({ event_type: "done" })}\n`);
      await writeFile(join(trialDir, "candidate.patch"), "diff --git a/file b/file\n");

      const uploads = await collectRuntimeArtifactUploads(runRoot, coreRunId);

      expect(uploads.omitted).toEqual([]);
      expect(uploads.items.map((item) => item.role)).toEqual([
        "agent_result",
        "agent_stdout",
        "agent_stderr",
        "agent_events",
        "candidate_patch",
        "contract_trace",
        "trial_summary",
      ]);
      expect(uploads.items[0]).toMatchObject({
        coreRunId,
        trialId: "trial-from-contract",
        scheduleIdx: 7,
        attempt: 2,
        role: "agent_result",
        relativePath: "agent/result.json",
        mediaType: "application/json; charset=utf-8",
      });
      expect(await readFile(uploads.items[0]!.absolutePath, "utf8")).toContain("Peter Gregory generated answer");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("bounds runtime snapshots across many large trials", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-budget-"));
    try {
      const runRoot = join(root, "run-root");
      const coreRunId = "run_20260529_000001_000001_000001";
      const runtimeDir = join(runRoot, coreRunId, "runtime");
      await mkdir(runtimeDir, { recursive: true });
      await writeFile(join(runtimeDir, "run_control.json"), JSON.stringify({ run_id: coreRunId, status: "completed" }));
      await writeFile(join(runtimeDir, "schedule_progress.json"), JSON.stringify({ committed: 200, total: 200 }));

      const largeMessage = "x".repeat(96 * 1024);
      for (let i = 0; i < 80; i += 1) {
        const trialId = `trial-${String(i).padStart(3, "0")}`;
        const trialDir = join(runRoot, coreRunId, "trials", trialId);
        await mkdir(join(trialDir, "agent"), { recursive: true });
        await writeFile(join(trialDir, "summary.json"), JSON.stringify({ outcome: "success", index: i }));
        await writeFile(join(trialDir, "agent", "events.jsonl"), `${JSON.stringify({
          event_type: "large",
          message: largeMessage,
        })}\n`);
      }

      const snapshot = await collectRuntimeSnapshot(runRoot, coreRunId);
      const snapshotBytes = Buffer.byteLength(JSON.stringify(snapshot), "utf8");

      expect(snapshotBytes).toBeLessThanOrEqual(4 * 1024 * 1024);
      expect(snapshot.trial_summaries.length).toBe(80);
      expect(snapshot.trial_summaries.filter((trial) => trial.trial_events).length).toBeLessThan(80);
      expect(snapshot.omitted.some((path) => path.endsWith("/events.jsonl"))).toBe(true);
      expect(snapshot.snapshot_budget.max_payload_bytes).toBe(4 * 1024 * 1024);
      expect(snapshot.snapshot_budget.envelope_reserve_bytes).toBe(128 * 1024);
      expect(Number(snapshot.snapshot_budget.estimated_payload_bytes)).toBeLessThanOrEqual(4 * 1024 * 1024);
      expect(JSON.stringify(snapshot)).not.toContain(root);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("redacts secret-looking fields from worker runtime snapshots", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-redact-"));
    try {
      const runRoot = join(root, "run-root");
      const coreRunId = "run_20260529_000001_000001_000001";
      const runtimeDir = join(runRoot, coreRunId, "runtime");
      const trialDir = join(runRoot, coreRunId, "trials", "trial-1");
      await mkdir(runtimeDir, { recursive: true });
      await mkdir(join(trialDir, "agent"), { recursive: true });
      await writeFile(join(runtimeDir, "run_control.json"), JSON.stringify({
        run_id: coreRunId,
        status: "completed",
        access_token: "worker-token-value",
      }));
      await writeFile(join(trialDir, "summary.json"), JSON.stringify({
        outcome: "success",
        metrics: {
          value: 1,
          api_key: "sk-secretsecretsecretsecretsecret",
        },
      }));
      await writeFile(join(trialDir, "agent", "events.jsonl"), `${JSON.stringify({
        event_type: "step",
        message: "safe message",
        secret_ref: "gcp-secret-manager://projects/acme/secrets/openai/versions/1",
      })}\n`);

      const snapshot = await collectRuntimeSnapshot(runRoot, coreRunId);
      const text = JSON.stringify(snapshot);

      expect(snapshot.runtime_values.run_control_v2?.access_token).toBe("[redacted]");
      expect(snapshot.trial_summaries[0]?.summary.metrics).toMatchObject({
        value: 1,
        api_key: "[redacted]",
      });
      expect(snapshot.trial_summaries[0]?.trial_events?.[0]).toMatchObject({
        event_type: "step",
        message: "safe message",
        secret_ref: "[redacted]",
      });
      expect(text).not.toContain("worker-token-value");
      expect(text).not.toContain("sk-secret");
      expect(text).not.toContain("gcp-secret-manager://");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("runner config does not require database credentials", () => {
    const config = loadWorkerConfig({
      BUCEPHALUS_CLOUD_API_URL: "https://cloud.example",
      BUCEPHALUS_CLOUD_WORKER_TOKEN: "worker-token",
      BUCEPHALUS_RUNNER_POOL_ID: "pool-1",
      BUCEPHALUS_WORKER_MIN_FREE_BYTES: "1",
      BUCEPHALUS_WORKER_IMAGE_REF: "us-central1-docker.pkg.dev/project/repo/worker@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    });

    expect(config.apiUrl).toBe("https://cloud.example");
    expect(config.runnerPoolId).toBe("pool-1");
    expect(config.workerImageRef).toBe("us-central1-docker.pkg.dev/project/repo/worker@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    expect(config.coreTimeoutMs).toBe(15 * 60 * 1000);
    expect(config.coreCompletionGraceMs).toBe(120_000);
    expect(config.capabilities.resources).not.toContain("network_perimeter");
  });

  test("runner config allows an explicit Core process timeout", () => {
    const config = loadWorkerConfig({
      BUCEPHALUS_CLOUD_API_URL: "https://cloud.example",
      BUCEPHALUS_CLOUD_WORKER_TOKEN: "worker-token",
      BUCEPHALUS_RUNNER_POOL_ID: "pool-1",
      BUCEPHALUS_WORKER_CORE_TIMEOUT_MS: "12345",
    });

    expect(config.coreTimeoutMs).toBe(12345);
  });

  test("runner config requires an explicit Cloud API URL", () => {
    expect(() => loadWorkerConfig({
      BUCEPHALUS_CLOUD_WORKER_TOKEN: "worker-token",
      BUCEPHALUS_RUNNER_POOL_ID: "pool-1",
      BUCEPHALUS_WORKER_MIN_FREE_BYTES: "1",
    })).toThrow("BUCEPHALUS_CLOUD_API_URL is required");
  });

  test("core runner env keeps Modal controls while stripping generic cloud credentials", () => {
    const previous = {
      BUCEPHALUS_MODAL_LAUNCHER: process.env.BUCEPHALUS_MODAL_LAUNCHER,
      BUCEPHALUS_MODAL_APP_NAME: process.env.BUCEPHALUS_MODAL_APP_NAME,
      BUCEPHALUS_MODAL_S3_ACCESS_KEY_ID: process.env.BUCEPHALUS_MODAL_S3_ACCESS_KEY_ID,
      AWS_ACCESS_KEY_ID: process.env.AWS_ACCESS_KEY_ID,
      AWS_SECRET_ACCESS_KEY: process.env.AWS_SECRET_ACCESS_KEY,
    };
    try {
      process.env.BUCEPHALUS_MODAL_LAUNCHER = "/usr/local/bin/bucephalus-modal-launcher";
      process.env.BUCEPHALUS_MODAL_APP_NAME = "bucephalus-prod";
      process.env.BUCEPHALUS_MODAL_S3_ACCESS_KEY_ID = "modal-sync-key";
      process.env.AWS_ACCESS_KEY_ID = "generic-aws-key";
      process.env.AWS_SECRET_ACCESS_KEY = "generic-aws-secret";

      const env = coreRunnerEnv();

      expect(env.BUCEPHALUS_MODAL_LAUNCHER).toBe("/usr/local/bin/bucephalus-modal-launcher");
      expect(env.BUCEPHALUS_MODAL_APP_NAME).toBe("bucephalus-prod");
      expect(env.BUCEPHALUS_MODAL_S3_ACCESS_KEY_ID).toBe("modal-sync-key");
      expect(env.AWS_ACCESS_KEY_ID).toBeUndefined();
      expect(env.AWS_SECRET_ACCESS_KEY).toBeUndefined();
    } finally {
      for (const [key, value] of Object.entries(previous)) {
        if (value === undefined) {
          delete process.env[key];
        } else {
          process.env[key] = value;
        }
      }
    }
  });

  test("core runner failure message preserves JSON stdout errors ahead of stderr progress logs", () => {
    const stdout = JSON.stringify({
      ok: false,
      error: {
        code: "command_failed",
        message:
          "local trial execution failed: modal sandbox launcher exited before emitting BUCEPHALUS_MODAL_RESULT",
      },
    });
    const stderr = [
      "preflight complete",
      "starting schedule execution: slots=36 max_concurrency=2",
    ].join("\n");

    const message = coreRunnerFailureMessage(1, stdout, stderr);

    expect(message).toContain("command_failed: local trial execution failed");
    expect(message).toContain(
      "modal sandbox launcher exited before emitting BUCEPHALUS_MODAL_RESULT",
    );
    expect(message).toContain("stderr tail: preflight complete");
  });

  test("core runner failure message preserves pretty JSON stdout errors", () => {
    const stdout = JSON.stringify(
      {
        command: "run",
        error: {
          code: "command_failed",
          message:
            "local trial execution failed (trial_id=trial_1, schedule_idx=0): required metric 'model_calls' resolved to null",
        },
        ok: false,
      },
      null,
      2,
    );
    const stderr = [
      "[preflight] [PASS] container_ready",
      "[run] run_20260614_002508_955023_000001: starting schedule execution: slots=18 max_concurrency=1",
    ].join("\n");

    const message = coreRunnerFailureMessage(1, stdout, stderr);

    expect(message).toContain("command_failed: local trial execution failed");
    expect(message).toContain("required metric 'model_calls' resolved to null");
    expect(message).toContain("stderr tail: [preflight] [PASS] container_ready");
  });

  test("Docker registry auth header decodes auth-only Docker config entries", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-docker-auth-"));
    const previousDockerConfig = process.env.DOCKER_CONFIG;
    try {
      await mkdir(root, { recursive: true });
      const credential = Buffer.from("oauth2accesstoken:ya29.token-value", "utf8").toString("base64");
      await writeFile(join(root, "config.json"), JSON.stringify({
        auths: {
          "us-central1-docker.pkg.dev": {
            auth: credential,
          },
        },
      }));
      process.env.DOCKER_CONFIG = root;

      const headers = await dockerRegistryAuthHeaders(
        "us-central1-docker.pkg.dev/project/repo/image@sha256:abc123",
      );
      const auth = JSON.parse(Buffer.from(headers["X-Registry-Auth"] ?? "", "base64").toString("utf8"));

      expect(auth).toEqual({
        username: "oauth2accesstoken",
        password: "ya29.token-value",
        auth: credential,
        serveraddress: "us-central1-docker.pkg.dev",
      });
    } finally {
      if (previousDockerConfig === undefined) {
        delete process.env.DOCKER_CONFIG;
      } else {
        process.env.DOCKER_CONFIG = previousDockerConfig;
      }
      await rm(root, { recursive: true, force: true });
    }
  });

  test("audited image pre-pull emits ImagePull lifecycle events", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-image-pull-"));
    const imageRef = "us-central1-docker.pkg.dev/project/repo/image@sha256:abc123";
    const events: JsonObject[] = [];
    const pulled: string[] = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "POST" && url.pathname === "/v1/worker/run-attempts/attempt-1/events") {
          expect(request.headers.get("authorization")).toBe("Bearer attempt-token");
          events.push(await request.json() as JsonObject);
          return Response.json({ event: { event_type: "accepted" } }, { status: 201 });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await prePullRunImagesWithAudit(
        runtimeAccessWorkerConfig(root, server.url.href, {
          capabilities: { executors: ["runner-docker"], resources: ["docker_daemon", "registry_pull"] },
        }),
        claim({
          run_requirements: {
            image_refs: [imageRef, imageRef],
          },
        }),
        undefined,
        async (ref) => {
          pulled.push(ref);
        },
      );

      expect(pulled).toEqual([imageRef]);
      expect(events.map((event) => event.event_type)).toEqual([
        "worker.runtime.image_pull.pulling",
        "worker.runtime.image_pull.pulled",
      ]);
      expect(events[0]).toMatchObject({
        runner_instance_id: "runner-instance-1",
        event_type: "worker.runtime.image_pull.pulling",
        payload: {
          resource_kind: "ImagePull",
          resource_name: "us-central1-docker.pkg.dev-project-repo-image-sha256-abc123",
          image_ref: imageRef,
          status: "pulling",
          attempt_id: "attempt-1",
          run_id: "run-1",
          runner_instance_id: "runner-instance-1",
          worker_id: "worker-1",
        },
      });
      expect(events[1]).toMatchObject({
        runner_instance_id: "runner-instance-1",
        event_type: "worker.runtime.image_pull.pulled",
        payload: {
          resource_kind: "ImagePull",
          resource_name: "us-central1-docker.pkg.dev-project-repo-image-sha256-abc123",
          image_ref: imageRef,
          status: "pulled",
        },
      });
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("audited image pre-pull records failed ImagePull lifecycle events", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-image-pull-failed-"));
    const imageRef = "us-central1-docker.pkg.dev/project/repo/image@sha256:bad";
    const events: JsonObject[] = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "POST" && url.pathname === "/v1/worker/run-attempts/attempt-1/events") {
          events.push(await request.json() as JsonObject);
          return Response.json({ event: { event_type: "accepted" } }, { status: 201 });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await expect(prePullRunImagesWithAudit(
        runtimeAccessWorkerConfig(root, server.url.href, {
          capabilities: { executors: ["runner-docker"], resources: ["docker_daemon", "registry_pull"] },
        }),
        claim({
          run_requirements: {
            image_refs: [imageRef],
          },
        }),
        undefined,
        async () => {
          throw new Error("registry unavailable");
        },
      )).rejects.toThrow("registry unavailable");

      expect(events.map((event) => event.event_type)).toEqual([
        "worker.runtime.image_pull.pulling",
        "worker.runtime.image_pull.failed",
      ]);
      expect(events[1]).toMatchObject({
        event_type: "worker.runtime.image_pull.failed",
        payload: {
          resource_kind: "ImagePull",
          resource_name: "us-central1-docker.pkg.dev-project-repo-image-sha256-bad",
          status: "failed",
          error: "registry unavailable",
        },
      });
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("audited sidecar validation emits SidecarRequirement lifecycle events", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-sidecar-requirement-"));
    const events: JsonObject[] = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "POST" && url.pathname === "/v1/worker/run-attempts/attempt-1/events") {
          expect(request.headers.get("authorization")).toBe("Bearer attempt-token");
          events.push(await request.json() as JsonObject);
          return Response.json({ event: { event_type: "accepted" } }, { status: 201 });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await validateSidecarRequirementsWithAudit(
        runtimeAccessWorkerConfig(root, server.url.href, {
          capabilities: { executors: ["runner-docker"], resources: ["sidecar:redis"] },
        }),
        claim({
          run_requirements: {
            sidecars: ["redis", "redis"],
          },
        }),
      );

      expect(events.map((event) => event.event_type)).toEqual([
        "worker.runtime.sidecar_requirement.checking",
        "worker.runtime.sidecar_requirement.available",
      ]);
      expect(events[0]).toMatchObject({
        runner_instance_id: "runner-instance-1",
        event_type: "worker.runtime.sidecar_requirement.checking",
        payload: {
          resource_kind: "SidecarRequirement",
          resource_name: "redis",
          sidecar: "redis",
          required_capability: "sidecar:redis",
          status: "checking",
          attempt_id: "attempt-1",
          run_id: "run-1",
          runner_instance_id: "runner-instance-1",
          worker_id: "worker-1",
        },
      });
      expect(events[1]).toMatchObject({
        runner_instance_id: "runner-instance-1",
        event_type: "worker.runtime.sidecar_requirement.available",
        payload: {
          resource_kind: "SidecarRequirement",
          resource_name: "redis",
          sidecar: "redis",
          required_capability: "sidecar:redis",
          status: "available",
        },
      });
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("audited sidecar validation records failed SidecarRequirement lifecycle events", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-sidecar-requirement-failed-"));
    const events: JsonObject[] = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "POST" && url.pathname === "/v1/worker/run-attempts/attempt-1/events") {
          events.push(await request.json() as JsonObject);
          return Response.json({ event: { event_type: "accepted" } }, { status: 201 });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await expect(validateSidecarRequirementsWithAudit(
        runtimeAccessWorkerConfig(root, server.url.href, {
          capabilities: { executors: ["runner-docker"], resources: [] },
        }),
        claim({
          run_requirements: {
            sidecars: ["redis"],
          },
        }),
      )).rejects.toThrow("Runner worker does not advertise required sidecar:redis");

      expect(events.map((event) => event.event_type)).toEqual([
        "worker.runtime.sidecar_requirement.checking",
        "worker.runtime.sidecar_requirement.failed",
      ]);
      expect(events[1]).toMatchObject({
        event_type: "worker.runtime.sidecar_requirement.failed",
        payload: {
          resource_kind: "SidecarRequirement",
          resource_name: "redis",
          sidecar: "redis",
          required_capability: "sidecar:redis",
          status: "failed",
          error: "Runner worker does not advertise required sidecar:redis",
        },
      });
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("audited accelerator validation emits AcceleratorRequirement lifecycle events", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-accelerator-requirement-"));
    const events: JsonObject[] = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "POST" && url.pathname === "/v1/worker/run-attempts/attempt-1/events") {
          expect(request.headers.get("authorization")).toBe("Bearer attempt-token");
          events.push(await request.json() as JsonObject);
          return Response.json({ event: { event_type: "accepted" } }, { status: 201 });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await validateAcceleratorRequirementsWithAudit(
        runtimeAccessWorkerConfig(root, server.url.href, {
          capabilities: { executors: ["runner-docker"], resources: ["accelerator:gpu-a10"] },
        }),
        claim({
          run_requirements: {
            accelerators: ["gpu-a10", "gpu-a10"],
          },
        }),
      );

      expect(events.map((event) => event.event_type)).toEqual([
        "worker.runtime.accelerator_requirement.checking",
        "worker.runtime.accelerator_requirement.available",
      ]);
      expect(events[0]).toMatchObject({
        runner_instance_id: "runner-instance-1",
        event_type: "worker.runtime.accelerator_requirement.checking",
        payload: {
          resource_kind: "AcceleratorRequirement",
          resource_name: "gpu-a10",
          accelerator: "gpu-a10",
          required_capability: "accelerator:gpu-a10",
          status: "checking",
          attempt_id: "attempt-1",
          run_id: "run-1",
          runner_instance_id: "runner-instance-1",
          worker_id: "worker-1",
        },
      });
      expect(events[1]).toMatchObject({
        runner_instance_id: "runner-instance-1",
        event_type: "worker.runtime.accelerator_requirement.available",
        payload: {
          resource_kind: "AcceleratorRequirement",
          resource_name: "gpu-a10",
          accelerator: "gpu-a10",
          required_capability: "accelerator:gpu-a10",
          status: "available",
        },
      });
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("audited accelerator validation records failed AcceleratorRequirement lifecycle events", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-accelerator-requirement-failed-"));
    const events: JsonObject[] = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "POST" && url.pathname === "/v1/worker/run-attempts/attempt-1/events") {
          events.push(await request.json() as JsonObject);
          return Response.json({ event: { event_type: "accepted" } }, { status: 201 });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await expect(validateAcceleratorRequirementsWithAudit(
        runtimeAccessWorkerConfig(root, server.url.href, {
          capabilities: { executors: ["runner-docker"], resources: [] },
        }),
        claim({
          run_requirements: {
            accelerators: ["gpu-a10"],
          },
        }),
      )).rejects.toThrow("Runner worker does not advertise required accelerator:gpu-a10");

      expect(events.map((event) => event.event_type)).toEqual([
        "worker.runtime.accelerator_requirement.checking",
        "worker.runtime.accelerator_requirement.failed",
      ]);
      expect(events[1]).toMatchObject({
        event_type: "worker.runtime.accelerator_requirement.failed",
        payload: {
          resource_kind: "AcceleratorRequirement",
          resource_name: "gpu-a10",
          accelerator: "gpu-a10",
          required_capability: "accelerator:gpu-a10",
          status: "failed",
          error: "Runner worker does not advertise required accelerator:gpu-a10",
        },
      });
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("worker verifies downloaded package digest before using extracted content", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-package-"));
    const serverState = {
      packageBytes: new Uint8Array(),
      authorization: null as string | null,
      attemptId: null as string | null,
    };
    const server = Bun.serve({
      port: 0,
      fetch(request) {
        if (new URL(request.url).pathname.startsWith("/v1/packages/")) {
          serverState.authorization = request.headers.get("authorization");
          serverState.attemptId = request.headers.get("x-bucephalus-attempt-id");
          return new Response(serverState.packageBytes, {
            headers: {
              "content-type": "application/gzip",
            },
          });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      const { archivePath } = await writeMinimalSealedPackage(root);
      serverState.packageBytes = await readFile(archivePath);

      await expect(materializePackage(
        {
          apiUrl: server.url.href.replace(/\/+$/, ""),
          workerId: "worker-1",
          runnerPoolId: "pool-1",
          runnerInstanceId: "runner-instance-1",
          leaseSeconds: 30,
          pollMs: 1000,
          heartbeatMs: 1000,
          sweeperMs: 1000,
          dataDir: root,
          coreRunnerCommand: "bucephalus",
          workerToken: "worker-token",
          secretResolverCommand: null,
          networkPolicyCommand: null,
          portForwardCommand: null,
          execCommand: null,
          capabilities: { executors: [], resources: [] },
          minFreeBytes: 0,
          retainAttemptWorkspaces: false,
          provisionRequestId: null,
          providerInstanceId: null,
          workerImageRef: null,
          liveEvidence: true,
          evidenceIntervalMs: 2000,
          coreTimeoutMs: 15 * 60 * 1000,
          coreCompletionGraceMs: 120_000,
          apiRequestTimeoutMs: 30_000,
        },
        {
          claimed: true,
          run: {
            run_id: "run-1",
            package_digest: "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            env: {},
            secret_refs: {},
            runtime_options: {},
            run_requirements: {
              executor: "runner-docker",
              requires: [],
              image_refs: [],
            },
          },
          attempt: {
            attempt_id: "attempt-1",
            attempt_token: "attempt-token",
          },
        },
      )).rejects.toThrow("Downloaded package digest mismatch");
      expect(serverState.authorization).toBe("Bearer attempt-token");
      expect(serverState.attemptId).toBe("attempt-1");
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("network perimeter capability requires an explicit policy command", () => {
    const config = loadWorkerConfig({
      BUCEPHALUS_CLOUD_API_URL: "https://cloud.example",
      BUCEPHALUS_CLOUD_WORKER_TOKEN: "worker-token",
      BUCEPHALUS_RUNNER_POOL_ID: "pool-1",
      BUCEPHALUS_WORKER_MIN_FREE_BYTES: "1",
      BUCEPHALUS_WORKER_NETWORK_POLICY_CMD_JSON: "[\"network-policy\"]",
    });

    expect(config.capabilities.resources).toContain("network_perimeter");
  });

  test("runtime access capabilities require explicit worker commands", () => {
    const baseEnv = {
      BUCEPHALUS_CLOUD_API_URL: "https://cloud.example",
      BUCEPHALUS_CLOUD_WORKER_TOKEN: "worker-token",
      BUCEPHALUS_RUNNER_POOL_ID: "pool-1",
      BUCEPHALUS_WORKER_MIN_FREE_BYTES: "1",
    };

    const defaultConfig = loadWorkerConfig(baseEnv);
    expect(defaultConfig.capabilities.resources).not.toContain("runtime_port_forward");
    expect(defaultConfig.capabilities.resources).not.toContain("runtime_exec");

    const accessConfig = loadWorkerConfig({
      ...baseEnv,
      BUCEPHALUS_WORKER_PORT_FORWARD_CMD_JSON: JSON.stringify(["port-forward-helper"]),
      BUCEPHALUS_WORKER_EXEC_CMD_JSON: JSON.stringify(["exec-helper"]),
    });
    expect(accessConfig.capabilities.resources).toContain("runtime_port_forward");
    expect(accessConfig.capabilities.resources).toContain("runtime_exec");
  });

  test("command-backed worker capabilities cannot be declared without matching helper commands", () => {
    const baseEnv = {
      BUCEPHALUS_CLOUD_API_URL: "https://cloud.example",
      BUCEPHALUS_CLOUD_WORKER_TOKEN: "worker-token",
      BUCEPHALUS_RUNNER_POOL_ID: "pool-1",
      BUCEPHALUS_WORKER_MIN_FREE_BYTES: "1",
    };
    const cases: Array<[string, string]> = [
      ["secret_resolver", "BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON"],
      ["network_perimeter", "BUCEPHALUS_WORKER_NETWORK_POLICY_CMD_JSON"],
      ["runtime_port_forward", "BUCEPHALUS_WORKER_PORT_FORWARD_CMD_JSON"],
      ["runtime_exec", "BUCEPHALUS_WORKER_EXEC_CMD_JSON"],
    ];

    for (const [resource, commandEnvName] of cases) {
      expect(() => loadWorkerConfig({
        ...baseEnv,
        BUCEPHALUS_WORKER_RESOURCES: `core_runner,${resource}`,
      })).toThrow(`${resource} requires ${commandEnvName} to be configured`);
    }
  });

  test("worker processes pending exec runtime resources through the resource API", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-exec-"));
    const updates: Array<{ path: string; body: JsonObject }> = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "GET" && url.pathname === "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec") {
          expect(request.headers.get("authorization")).toBe("Bearer attempt-token");
          expect(url.searchParams.get("runner_instance_id")).toBe("runner-instance-1");
          return Response.json({
            resources: [
              {
                kind: "Exec",
                metadata: {
                  name: "exec-1",
                  uid: "exec-1",
                  labels: {
                    "bucephalus.dev/run-id": "run-1",
                  },
                },
                spec: {
                  access_request_id: "exec-1",
                  resource_kind: "RunnerInstance",
                  resource_name: "runner-1",
                  protocol: "exec",
                  command: ["whoami"],
                },
                status: {
                  phase: "requested",
                  runner_instance_id: "runner-instance-1",
                  attempt_id: "attempt-1",
                  connection: {},
                },
                audit: {
                  requester: "issuer:user-a",
                },
              },
            ],
          });
        }
        if (request.method === "POST" && url.pathname.startsWith("/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/")) {
          updates.push({
            path: url.pathname,
            body: await request.json() as JsonObject,
          });
          return Response.json({ resource: { kind: "Exec", metadata: { name: "exec-1" } } });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await processExecRequests(
        {
          apiUrl: server.url.href.replace(/\/+$/, ""),
          workerId: "worker-1",
          runnerPoolId: "pool-1",
          runnerInstanceId: "runner-instance-1",
          leaseSeconds: 30,
          pollMs: 1000,
          heartbeatMs: 1000,
          sweeperMs: 1000,
          dataDir: root,
          coreRunnerCommand: "bucephalus",
          workerToken: "worker-token",
          secretResolverCommand: null,
          networkPolicyCommand: null,
          portForwardCommand: null,
          execCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/runtimeExec.ts")],
          capabilities: { executors: ["runner-docker"], resources: ["runtime_exec"] },
          minFreeBytes: 0,
          retainAttemptWorkspaces: false,
          provisionRequestId: null,
          providerInstanceId: null,
          workerImageRef: null,
          liveEvidence: true,
          evidenceIntervalMs: 2000,
          coreTimeoutMs: 15 * 60 * 1000,
          coreCompletionGraceMs: 120_000,
          apiRequestTimeoutMs: 30_000,
        },
        {
          run: {
            run_id: "run-1",
            package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            env: {},
            secret_refs: {},
            runtime_options: {},
            run_requirements: {
              executor: "runner-docker",
              requires: [],
              image_refs: [],
            },
          },
          attempt: {
            attempt_id: "attempt-1",
            attempt_token: "attempt-token",
          },
        },
        {
          workspaceDir: root,
          extractedDir: join(root, "package"),
          runRootDir: join(root, "run-root"),
        },
      );

      expect(updates.map((item) => item.path)).toEqual([
        "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/accept",
        "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/complete",
      ]);
      expect(updates[0]?.body).toMatchObject({
        runner_instance_id: "runner-instance-1",
        connection: {
          mode: "worker_command",
          worker_id: "worker-1",
        },
      });
      expect(updates[1]?.body).toMatchObject({
        runner_instance_id: "runner-instance-1",
        connection: {
          mode: "worker_command",
          worker_id: "worker-1",
          exit_code: 0,
          stdout_tail: "hello from exec\n",
          stdout_bytes: 16,
          stdout_tail_bytes: 16,
          stdout_tail_truncated: false,
          stderr_tail: "",
          stderr_bytes: 0,
          stderr_tail_bytes: 0,
          stderr_tail_truncated: false,
        },
      });
      const commandInput = JSON.parse(await readFile(join(root, "exec-input.json"), "utf8"));
      expect(commandInput.exec).toMatchObject({
        access_request_id: "exec-1",
        resource_kind: "RunnerInstance",
        resource_name: "runner-1",
        command: ["whoami"],
      });
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("worker reports runtime exec output truncation evidence", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-exec-long-output-"));
    const updates: Array<{ path: string; body: JsonObject }> = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "GET" && url.pathname === "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec") {
          return Response.json({
            resources: [
              {
                kind: "Exec",
                metadata: {
                  name: "exec-1",
                  uid: "exec-1",
                  labels: {
                    "bucephalus.dev/run-id": "run-1",
                  },
                },
                spec: {
                  access_request_id: "exec-1",
                  resource_kind: "RunnerInstance",
                  resource_name: "runner-1",
                  protocol: "exec",
                  command: ["long-output"],
                },
                status: {
                  phase: "requested",
                  runner_instance_id: "runner-instance-1",
                  attempt_id: "attempt-1",
                  connection: {},
                },
              },
            ],
          });
        }
        if (request.method === "POST" && url.pathname.startsWith("/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/")) {
          updates.push({
            path: url.pathname,
            body: await request.json() as JsonObject,
          });
          return Response.json({ resource: { kind: "Exec", metadata: { name: "exec-1" } } });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await processExecRequests(
        runtimeAccessWorkerConfig(root, server.url.href, {
          execCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/runtimeExecLongOutput.ts")],
          capabilities: { executors: ["runner-docker"], resources: ["runtime_exec"] },
        }),
        claim(),
        {
          workspaceDir: root,
          extractedDir: join(root, "package"),
          runRootDir: join(root, "run-root"),
        },
      );

      expect(updates.map((item) => item.path)).toEqual([
        "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/accept",
        "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/complete",
      ]);
      expect(updates[1]?.body).toMatchObject({
        runner_instance_id: "runner-instance-1",
        connection: {
          mode: "worker_command",
          worker_id: "worker-1",
          exit_code: 0,
          stdout_bytes: 20_000,
          stdout_tail_bytes: 16_000,
          stdout_tail_truncated: true,
          stderr_tail: "warn\n",
          stderr_bytes: 5,
          stderr_tail_bytes: 5,
          stderr_tail_truncated: false,
        },
      });
      expect(String((updates[1]?.body.connection as JsonObject | undefined)?.stdout_tail ?? "").length).toBe(16_000);
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("worker processes pending port-forward runtime resources through the resource API", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-port-forward-"));
    const updates: Array<{ path: string; body: JsonObject }> = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "GET" && url.pathname === "/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward") {
          expect(request.headers.get("authorization")).toBe("Bearer attempt-token");
          expect(url.searchParams.get("runner_instance_id")).toBe("runner-instance-1");
          return Response.json({
            resources: [
              {
                kind: "PortForward",
                metadata: {
                  name: "pf-1",
                  uid: "pf-1",
                  labels: {
                    "bucephalus.dev/run-id": "run-1",
                  },
                },
                spec: {
                  access_request_id: "pf-1",
                  resource_kind: "Trial",
                  resource_name: "trial-1.0",
                  protocol: "tcp",
                  target_port: 8080,
                  local_port: 18080,
                },
                status: {
                  phase: "requested",
                  runner_instance_id: "runner-instance-1",
                  attempt_id: "attempt-1",
                  connection: {},
                },
                audit: {
                  requester: "issuer:user-a",
                },
              },
            ],
          });
        }
        if (request.method === "POST" && url.pathname.startsWith("/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward/pf-1/")) {
          updates.push({
            path: url.pathname,
            body: await request.json() as JsonObject,
          });
          return Response.json({ resource: { kind: "PortForward", metadata: { name: "pf-1" } } });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await processPortForwardRequests(
        {
          apiUrl: server.url.href.replace(/\/+$/, ""),
          workerId: "worker-1",
          runnerPoolId: "pool-1",
          runnerInstanceId: "runner-instance-1",
          leaseSeconds: 30,
          pollMs: 1000,
          heartbeatMs: 1000,
          sweeperMs: 1000,
          dataDir: root,
          coreRunnerCommand: "bucephalus",
          workerToken: "worker-token",
          secretResolverCommand: null,
          networkPolicyCommand: null,
          portForwardCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/portForward.ts")],
          execCommand: null,
          capabilities: { executors: ["runner-docker"], resources: ["runtime_port_forward"] },
          minFreeBytes: 0,
          retainAttemptWorkspaces: false,
          provisionRequestId: null,
          providerInstanceId: null,
          workerImageRef: null,
          liveEvidence: true,
          evidenceIntervalMs: 2000,
          coreTimeoutMs: 15 * 60 * 1000,
          coreCompletionGraceMs: 120_000,
          apiRequestTimeoutMs: 30_000,
        },
        {
          run: {
            run_id: "run-1",
            package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            env: {},
            secret_refs: {},
            runtime_options: {},
            run_requirements: {
              executor: "runner-docker",
              requires: [],
              image_refs: [],
            },
          },
          attempt: {
            attempt_id: "attempt-1",
            attempt_token: "attempt-token",
          },
        },
        {
          workspaceDir: root,
          extractedDir: join(root, "package"),
          runRootDir: join(root, "run-root"),
        },
      );

      expect(updates.map((item) => item.path)).toEqual([
        "/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward/pf-1/accept",
        "/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward/pf-1/active",
      ]);
      expect(updates[0]?.body).toMatchObject({
        runner_instance_id: "runner-instance-1",
        connection: {
          mode: "worker_command",
          worker_id: "worker-1",
        },
      });
      expect(updates[1]?.body).toMatchObject({
        runner_instance_id: "runner-instance-1",
        connection: {
          kind: "loopback",
          target: "tcp:8080",
          local_port: 18080,
          client_reachable: true,
          client_endpoint: "tcp://127.0.0.1:18080",
        },
      });
      const commandInput = JSON.parse(await readFile(join(root, "port-forward-input.json"), "utf8"));
      expect(commandInput.port_forward).toMatchObject({
        access_request_id: "pf-1",
        resource_kind: "Trial",
        resource_name: "trial-1.0",
        target_port: 8080,
        local_port: 18080,
      });
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("built-in GCE IAP port-forward helper reports an auditable runner VM tunnel handle", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-gce-iap-port-forward-"));
    try {
      const result = await runWorkerHelper(["runtime-gce-iap-port-forward"], {
        schema_version: "runtime_port_forward_command_v1",
        worker_id: "worker-1",
        runner_instance_id: "runner-instance-1",
        provider_instance_id: "gce://projects/project-1/zones/us-central1-a/instances/runner-vm-1",
        run_id: "run-1",
        attempt_id: "attempt-1",
        workspace_dir: root,
        package_dir: join(root, "package"),
        run_root_dir: join(root, "run-root"),
        port_forward: {
          access_request_id: "pf-1",
          resource_kind: "RunnerInstance",
          resource_name: "runner-vm-1",
          protocol: "tcp",
          target_port: 8080,
          local_port: 18080,
        },
      });

      expect(result.exitCode).toBe(0);
      expect(result.stderr).toBe("");
      const output = JSON.parse(result.stdout) as JsonObject;
      expect(output).toMatchObject({
        status: "active",
        connection: {
          mode: "gcp_iap_ssh",
          provider: "gce-iap-ssh-local-forward",
          project_id: "project-1",
          zone: "us-central1-a",
          instance_name: "runner-vm-1",
          target_host: "127.0.0.1",
          target_port: 8080,
          requested_target_port: 8080,
          local_port: 18080,
          target_mode: "worker_host",
          client_reachable: false,
          provider_tunnel_url: "gcp-iap-ssh://projects/project-1/zones/us-central1-a/instances/runner-vm-1?target_host=127.0.0.1&target_port=8080",
          tunnel: "gcp-iap-ssh runner-vm-1 127.0.0.1:8080",
          worker_id: "worker-1",
          resource_kind: "RunnerInstance",
          resource_name: "runner-vm-1",
        },
      });
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("built-in runtime exec helper runs commands against the runner worker process", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runner-exec-"));
    try {
      const result = await runWorkerHelper(["runtime-docker-exec"], {
        schema_version: "runtime_exec_command_v1",
        worker_id: "worker-1",
        runner_instance_id: "runner-instance-1",
        provider_instance_id: "gce://projects/project-1/zones/us-central1-a/instances/runner-vm-1",
        run_id: "run-1",
        attempt_id: "attempt-1",
        workspace_dir: root,
        package_dir: join(root, "package"),
        run_root_dir: join(root, "run-root"),
        exec: {
          access_request_id: "exec-1",
          resource_kind: "RunnerInstance",
          resource_name: "runner-vm-1",
          protocol: "exec",
          command: [process.execPath, "-e", "process.stdout.write('runner-exec-ok')"],
        },
      });

      expect(result.exitCode).toBe(0);
      expect(result.stderr).toBe("");
      const output = JSON.parse(result.stdout) as JsonObject;
      expect(output).toMatchObject({
        status: "completed",
        exit_code: 0,
        stdout: "runner-exec-ok",
        stderr: "",
        error_message: null,
        connection: {
          mode: "worker_process",
          provider: "gce-worker-container",
          resource_kind: "RunnerInstance",
          resource_name: "runner-vm-1",
        },
      });
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("worker fails port-forward requests when helper reports no usable connection handle", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-port-forward-empty-"));
    const updates: Array<{ path: string; body: JsonObject }> = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "GET" && url.pathname === "/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward") {
          return Response.json({
            resources: [
              {
                kind: "PortForward",
                metadata: {
                  name: "pf-1",
                  uid: "pf-1",
                  labels: {
                    "bucephalus.dev/run-id": "run-1",
                  },
                },
                spec: {
                  access_request_id: "pf-1",
                  resource_kind: "Trial",
                  resource_name: "trial-1.0",
                  protocol: "tcp",
                  target_port: 8080,
                  local_port: 18080,
                },
                status: {
                  phase: "requested",
                  runner_instance_id: "runner-instance-1",
                  attempt_id: "attempt-1",
                  connection: {},
                },
                audit: {},
              },
            ],
          });
        }
        if (request.method === "POST" && url.pathname.startsWith("/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward/pf-1/")) {
          updates.push({
            path: url.pathname,
            body: await request.json() as JsonObject,
          });
          return Response.json({ resource: { kind: "PortForward", metadata: { name: "pf-1" } } });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await processPortForwardRequests(
        runtimeAccessWorkerConfig(root, server.url.href, {
          portForwardCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/portForwardEmptyConnection.ts")],
          capabilities: { executors: ["runner-docker"], resources: ["runtime_port_forward"] },
        }),
        claim(),
        {
          workspaceDir: root,
          extractedDir: join(root, "package"),
          runRootDir: join(root, "run-root"),
        },
      );

      expect(updates.map((item) => item.path)).toEqual([
        "/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward/pf-1/accept",
        "/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward/pf-1/fail",
      ]);
      expect(updates[1]?.body).toMatchObject({
        runner_instance_id: "runner-instance-1",
        connection: {},
        error_message: "port-forward command did not report a usable connection handle",
      });
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("worker fails exec requests when helper omits exit code", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-exec-empty-"));
    const updates: Array<{ path: string; body: JsonObject }> = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "GET" && url.pathname === "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec") {
          return Response.json({
            resources: [
              {
                kind: "Exec",
                metadata: {
                  name: "exec-1",
                  uid: "exec-1",
                  labels: {
                    "bucephalus.dev/run-id": "run-1",
                  },
                },
                spec: {
                  access_request_id: "exec-1",
                  resource_kind: "RunnerInstance",
                  resource_name: "runner-1",
                  protocol: "exec",
                  command: ["whoami"],
                },
                status: {
                  phase: "requested",
                  runner_instance_id: "runner-instance-1",
                  attempt_id: "attempt-1",
                  connection: {},
                },
                audit: {},
              },
            ],
          });
        }
        if (request.method === "POST" && url.pathname.startsWith("/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/")) {
          updates.push({
            path: url.pathname,
            body: await request.json() as JsonObject,
          });
          return Response.json({ resource: { kind: "Exec", metadata: { name: "exec-1" } } });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await processExecRequests(
        runtimeAccessWorkerConfig(root, server.url.href, {
          execCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/runtimeExecEmptyResult.ts")],
          capabilities: { executors: ["runner-docker"], resources: ["runtime_exec"] },
        }),
        claim(),
        {
          workspaceDir: root,
          extractedDir: join(root, "package"),
          runRootDir: join(root, "run-root"),
        },
      );

      expect(updates.map((item) => item.path)).toEqual([
        "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/accept",
        "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/fail",
      ]);
      expect(updates[1]?.body).toMatchObject({
        runner_instance_id: "runner-instance-1",
        connection: {
          mode: "worker_command",
          worker_id: "worker-1",
          stdout_tail: "missing exit code\n",
          stderr_tail: "",
        },
        error_message: "runtime exec command did not report an exit_code",
      });
      expect((updates[1]?.body.connection as JsonObject).exit_code).toBeUndefined();
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("worker fails exec requests when helper output exceeds the control-plane JSON budget", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-exec-huge-"));
    const updates: Array<{ path: string; body: JsonObject }> = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "GET" && url.pathname === "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec") {
          return Response.json({
            resources: [
              {
                kind: "Exec",
                metadata: {
                  name: "exec-1",
                  uid: "exec-1",
                  labels: {
                    "bucephalus.dev/run-id": "run-1",
                  },
                },
                spec: {
                  access_request_id: "exec-1",
                  resource_kind: "RunnerInstance",
                  resource_name: "runner-1",
                  protocol: "exec",
                  command: ["whoami"],
                },
                status: {
                  phase: "requested",
                  runner_instance_id: "runner-instance-1",
                  attempt_id: "attempt-1",
                  connection: {},
                },
                audit: {},
              },
            ],
          });
        }
        if (request.method === "POST" && url.pathname.startsWith("/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/")) {
          updates.push({
            path: url.pathname,
            body: await request.json() as JsonObject,
          });
          return Response.json({ resource: { kind: "Exec", metadata: { name: "exec-1" } } });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await processExecRequests(
        runtimeAccessWorkerConfig(root, server.url.href, {
          execCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/runtimeExecHugeOutput.ts")],
          capabilities: { executors: ["runner-docker"], resources: ["runtime_exec"] },
        }),
        claim(),
        {
          workspaceDir: root,
          extractedDir: join(root, "package"),
          runRootDir: join(root, "run-root"),
        },
      );

      expect(updates.map((item) => item.path)).toEqual([
        "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/accept",
        "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/fail",
      ]);
      expect(updates[1]?.body).toMatchObject({
        runner_instance_id: "runner-instance-1",
        error_message: expect.stringContaining("command stdout exceeded 1048576 bytes"),
      });
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("worker treats runtime access transition conflicts as stale control-plane state", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-exec-stale-"));
    const updates: Array<{ path: string; body: JsonObject }> = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "GET" && url.pathname === "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec") {
          return Response.json({
            resources: [
              {
                kind: "Exec",
                metadata: {
                  name: "exec-1",
                  uid: "exec-1",
                  labels: {
                    "bucephalus.dev/run-id": "run-1",
                  },
                },
                spec: {
                  access_request_id: "exec-1",
                  resource_kind: "RunnerInstance",
                  resource_name: "runner-1",
                  protocol: "exec",
                  command: ["whoami"],
                },
                status: {
                  phase: "requested",
                  runner_instance_id: "runner-instance-1",
                  attempt_id: "attempt-1",
                  connection: {},
                },
                audit: {},
              },
            ],
          });
        }
        if (request.method === "POST" && url.pathname === "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/accept") {
          updates.push({
            path: url.pathname,
            body: await request.json() as JsonObject,
          });
          return Response.json({
            code: "runtime_access_transition_invalid",
            message: "Exec request cannot transition from active to accepted",
            detail: {
              access_request_id: "exec-1",
              kind: "exec",
              current_status: "active",
              target_status: "accepted",
            },
          }, { status: 409 });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await processExecRequests(
        {
          apiUrl: server.url.href.replace(/\/+$/, ""),
          workerId: "worker-1",
          runnerPoolId: "pool-1",
          runnerInstanceId: "runner-instance-1",
          leaseSeconds: 30,
          pollMs: 1000,
          heartbeatMs: 1000,
          sweeperMs: 1000,
          dataDir: root,
          coreRunnerCommand: "bucephalus",
          workerToken: "worker-token",
          secretResolverCommand: null,
          networkPolicyCommand: null,
          portForwardCommand: null,
          execCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/runtimeExec.ts")],
          capabilities: { executors: ["runner-docker"], resources: ["runtime_exec"] },
          minFreeBytes: 0,
          retainAttemptWorkspaces: false,
          provisionRequestId: null,
          providerInstanceId: null,
          workerImageRef: null,
          liveEvidence: true,
          evidenceIntervalMs: 2000,
          coreTimeoutMs: 15 * 60 * 1000,
          coreCompletionGraceMs: 120_000,
          apiRequestTimeoutMs: 30_000,
        },
        {
          run: {
            run_id: "run-1",
            package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            env: {},
            secret_refs: {},
            runtime_options: {},
            run_requirements: {
              executor: "runner-docker",
              requires: [],
              image_refs: [],
            },
          },
          attempt: {
            attempt_id: "attempt-1",
            attempt_token: "attempt-token",
          },
        },
        {
          workspaceDir: root,
          extractedDir: join(root, "package"),
          runRootDir: join(root, "run-root"),
        },
      );

      expect(updates.map((item) => item.path)).toEqual([
        "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec/exec-1/accept",
      ]);
      await expect(readFile(join(root, "exec-input.json"), "utf8")).rejects.toThrow();
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("worker treats port-forward transition conflicts as stale control-plane state", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-port-forward-stale-"));
    const updates: Array<{ path: string; body: JsonObject }> = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "GET" && url.pathname === "/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward") {
          return Response.json({
            resources: [
              {
                kind: "PortForward",
                metadata: {
                  name: "pf-1",
                  uid: "pf-1",
                  labels: {
                    "bucephalus.dev/run-id": "run-1",
                  },
                },
                spec: {
                  access_request_id: "pf-1",
                  resource_kind: "Trial",
                  resource_name: "trial-1.0",
                  protocol: "tcp",
                  target_port: 8080,
                  local_port: 18080,
                },
                status: {
                  phase: "requested",
                  runner_instance_id: "runner-instance-1",
                  attempt_id: "attempt-1",
                  connection: {},
                },
                audit: {},
              },
            ],
          });
        }
        if (request.method === "POST" && url.pathname === "/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward/pf-1/accept") {
          updates.push({
            path: url.pathname,
            body: await request.json() as JsonObject,
          });
          return Response.json({
            code: "runtime_access_transition_invalid",
            message: "PortForward request cannot transition from active to accepted",
            detail: {
              access_request_id: "pf-1",
              kind: "port_forward",
              current_status: "active",
              target_status: "accepted",
            },
          }, { status: 409 });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await processPortForwardRequests(
        {
          apiUrl: server.url.href.replace(/\/+$/, ""),
          workerId: "worker-1",
          runnerPoolId: "pool-1",
          runnerInstanceId: "runner-instance-1",
          leaseSeconds: 30,
          pollMs: 1000,
          heartbeatMs: 1000,
          sweeperMs: 1000,
          dataDir: root,
          coreRunnerCommand: "bucephalus",
          workerToken: "worker-token",
          secretResolverCommand: null,
          networkPolicyCommand: null,
          portForwardCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/portForward.ts")],
          execCommand: null,
          capabilities: { executors: ["runner-docker"], resources: ["runtime_port_forward"] },
          minFreeBytes: 0,
          retainAttemptWorkspaces: false,
          provisionRequestId: null,
          providerInstanceId: null,
          workerImageRef: null,
          liveEvidence: true,
          evidenceIntervalMs: 2000,
          coreTimeoutMs: 15 * 60 * 1000,
          coreCompletionGraceMs: 120_000,
          apiRequestTimeoutMs: 30_000,
        },
        {
          run: {
            run_id: "run-1",
            package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            env: {},
            secret_refs: {},
            runtime_options: {},
            run_requirements: {
              executor: "runner-docker",
              requires: [],
              image_refs: [],
            },
          },
          attempt: {
            attempt_id: "attempt-1",
            attempt_token: "attempt-token",
          },
        },
        {
          workspaceDir: root,
          extractedDir: join(root, "package"),
          runRootDir: join(root, "run-root"),
        },
      );

      expect(updates.map((item) => item.path)).toEqual([
        "/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward/pf-1/accept",
      ]);
      await expect(readFile(join(root, "port-forward-input.json"), "utf8")).rejects.toThrow();
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("worker ignores wrong-kind runtime access resources from worker lists", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-access-kind-"));
    const updates: Array<{ path: string; body: JsonObject }> = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "GET" && url.pathname === "/v1/worker/run-attempts/attempt-1/runtime/resources/PortForward") {
          return Response.json({
            resources: [
              {
                kind: "exec",
                access_request_id: "exec-in-port-forward-list",
                run_id: "run-1",
                status: "requested",
                resource_kind: "RunnerInstance",
                resource_name: "runner-1",
                protocol: "exec",
                target_port: null,
                local_port: null,
                command: ["whoami"],
                runner_instance_id: "runner-instance-1",
                attempt_id: "attempt-1",
                connection: {},
                created_at: "2026-06-04T00:00:00Z",
                updated_at: "2026-06-04T00:00:00Z",
              },
              {
                kind: "PortForward",
                metadata: {
                  name: "pf-missing-target",
                  uid: "pf-missing-target",
                  labels: {
                    "bucephalus.dev/run-id": "run-1",
                  },
                },
                spec: {
                  access_request_id: "pf-missing-target",
                  protocol: "tcp",
                  target_port: 8080,
                },
                status: {
                  phase: "requested",
                  runner_instance_id: "runner-instance-1",
                  attempt_id: "attempt-1",
                  connection: {},
                },
              },
              {
                kind: "PortForward",
                metadata: {
                  name: "pf-invalid-port",
                  uid: "pf-invalid-port",
                  labels: {
                    "bucephalus.dev/run-id": "run-1",
                  },
                },
                spec: {
                  access_request_id: "pf-invalid-port",
                  resource_kind: "Trial",
                  resource_name: "trial-1.0",
                  protocol: "tcp",
                  target_port: 0,
                },
                status: {
                  phase: "requested",
                  runner_instance_id: "runner-instance-1",
                  attempt_id: "attempt-1",
                  connection: {},
                },
              },
            ],
          });
        }
        if (request.method === "GET" && url.pathname === "/v1/worker/run-attempts/attempt-1/runtime/resources/Exec") {
          return Response.json({
            resources: [
              {
                kind: "port_forward",
                access_request_id: "pf-in-exec-list",
                run_id: "run-1",
                status: "requested",
                resource_kind: "Trial",
                resource_name: "trial-1.0",
                protocol: "tcp",
                target_port: 8080,
                local_port: 18080,
                runner_instance_id: "runner-instance-1",
                attempt_id: "attempt-1",
                connection: {},
                created_at: "2026-06-04T00:00:00Z",
                updated_at: "2026-06-04T00:00:00Z",
              },
              {
                kind: "Exec",
                metadata: {
                  name: "exec-missing-target",
                  uid: "exec-missing-target",
                  labels: {
                    "bucephalus.dev/run-id": "run-1",
                  },
                },
                spec: {
                  access_request_id: "exec-missing-target",
                  protocol: "exec",
                  command: ["whoami"],
                },
                status: {
                  phase: "requested",
                  runner_instance_id: "runner-instance-1",
                  attempt_id: "attempt-1",
                  connection: {},
                },
              },
              {
                kind: "Exec",
                metadata: {
                  name: "exec-empty-command",
                  uid: "exec-empty-command",
                  labels: {
                    "bucephalus.dev/run-id": "run-1",
                  },
                },
                spec: {
                  access_request_id: "exec-empty-command",
                  resource_kind: "RunnerInstance",
                  resource_name: "runner-1",
                  protocol: "exec",
                  command: [],
                },
                status: {
                  phase: "requested",
                  runner_instance_id: "runner-instance-1",
                  attempt_id: "attempt-1",
                  connection: {},
                },
              },
            ],
          });
        }
        if (request.method === "POST" && url.pathname.startsWith("/v1/worker/run-attempts/attempt-1/runtime/resources/")) {
          updates.push({
            path: url.pathname,
            body: await request.json() as JsonObject,
          });
          return Response.json({ resource: { kind: "RuntimeAccess", metadata: { name: "unexpected" } } });
        }
        return new Response("not found", { status: 404 });
      },
    });
    const config = {
      apiUrl: server.url.href.replace(/\/+$/, ""),
      workerId: "worker-1",
      runnerPoolId: "pool-1",
      runnerInstanceId: "runner-instance-1",
      leaseSeconds: 30,
      pollMs: 1000,
      heartbeatMs: 1000,
      sweeperMs: 1000,
      dataDir: root,
      coreRunnerCommand: "bucephalus",
      workerToken: "worker-token",
      secretResolverCommand: null,
      networkPolicyCommand: null,
      portForwardCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/portForward.ts")],
      execCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/runtimeExec.ts")],
      capabilities: { executors: ["runner-docker"], resources: ["runtime_port_forward", "runtime_exec"] },
      minFreeBytes: 0,
      retainAttemptWorkspaces: false,
      provisionRequestId: null,
      providerInstanceId: null,
      workerImageRef: null,
      liveEvidence: true,
      evidenceIntervalMs: 2000,
      coreTimeoutMs: 15 * 60 * 1000,
      coreCompletionGraceMs: 120_000,
      apiRequestTimeoutMs: 30_000,
    };
    const claim = {
      run: {
        run_id: "run-1",
        package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        env: {},
        secret_refs: {},
        runtime_options: {},
        run_requirements: {
          executor: "runner-docker",
          requires: [],
          image_refs: [],
        },
      },
      attempt: {
        attempt_id: "attempt-1",
        attempt_token: "attempt-token",
      },
    };
    const materialized = {
      workspaceDir: root,
      extractedDir: join(root, "package"),
      runRootDir: join(root, "run-root"),
    };
    try {
      await processPortForwardRequests(config, claim, materialized);
      await processExecRequests(config, claim, materialized);

      expect(updates).toEqual([]);
      await expect(readFile(join(root, "port-forward-input.json"), "utf8")).rejects.toThrow();
      await expect(readFile(join(root, "exec-input.json"), "utf8")).rejects.toThrow();
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("network egress requirements require a configured policy enforcer", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-network-"));
    try {
      await expect(applyRuntimeNetworkPolicy(
        {
          networkPolicyCommand: null,
          workerId: "worker-1",
          runnerInstanceId: "runner-instance-1",
        },
        claimWithNetwork(["api.openai.com"]),
        {
          workspaceDir: root,
          runRootDir: join(root, "run-root"),
        },
      )).rejects.toThrow("no network policy enforcer is configured");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("network policy enforcer receives attempt-scoped egress requirements", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-network-"));
    try {
      await applyRuntimeNetworkPolicy(
        {
          networkPolicyCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/networkPolicy.ts")],
          workerId: "worker-1",
          runnerInstanceId: "runner-instance-1",
        },
        claimWithNetwork(["api.openai.com", "storage.googleapis.com"]),
        {
          workspaceDir: root,
          runRootDir: join(root, "run-root"),
        },
      );

      const input = JSON.parse(await readFile(join(root, "network-policy-input.json"), "utf8"));
      expect(input).toMatchObject({
        attempt_id: "attempt-1",
        run_id: "run-1",
        runner_instance_id: "runner-instance-1",
        worker_id: "worker-1",
        egress_hosts: ["api.openai.com", "storage.googleapis.com"],
        network_perimeter: {
          default: "none",
          task_sandbox: "none",
          agent: "none",
          egress_hosts: ["api.openai.com", "storage.googleapis.com"],
        },
      });
      expect(input.workspace_dir).toBe(root);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("audited network policy emits resource lifecycle events", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-network-audit-"));
    const events: JsonObject[] = [];
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "POST" && url.pathname === "/v1/worker/run-attempts/attempt-1/events") {
          expect(request.headers.get("authorization")).toBe("Bearer attempt-token");
          events.push(await request.json() as JsonObject);
          return Response.json({ event: { event_type: "accepted" } }, { status: 201 });
        }
        return new Response("not found", { status: 404 });
      },
    });
    try {
      await applyRuntimeNetworkPolicyWithAudit(
        runtimeAccessWorkerConfig(root, server.url.href, {
          networkPolicyCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/networkPolicy.ts")],
          capabilities: { executors: ["runner-docker"], resources: ["network_perimeter"] },
        }),
        claimWithNetwork(["api.openai.com", "storage.googleapis.com"]),
        {
          workspaceDir: root,
          runRootDir: join(root, "run-root"),
        },
      );

      expect(events.map((event) => event.event_type)).toEqual([
        "worker.runtime.network_perimeter.applying",
        "worker.runtime.network_perimeter.applied",
      ]);
      expect(events[0]).toMatchObject({
        runner_instance_id: "runner-instance-1",
        event_type: "worker.runtime.network_perimeter.applying",
        payload: {
          resource_kind: "NetworkPerimeter",
          resource_name: "declared",
          status: "applying",
          attempt_id: "attempt-1",
          run_id: "run-1",
          runner_instance_id: "runner-instance-1",
          worker_id: "worker-1",
          egress_hosts: ["api.openai.com", "storage.googleapis.com"],
        },
      });
      expect(events[1]).toMatchObject({
        runner_instance_id: "runner-instance-1",
        event_type: "worker.runtime.network_perimeter.applied",
        payload: {
          resource_kind: "NetworkPerimeter",
          resource_name: "declared",
          status: "applied",
        },
      });
      expect(JSON.stringify(events)).not.toContain("secret-manager");
      const input = JSON.parse(await readFile(join(root, "network-policy-input.json"), "utf8"));
      expect(input.egress_hosts).toEqual(["api.openai.com", "storage.googleapis.com"]);
    } finally {
      server.stop(true);
      await rm(root, { recursive: true, force: true });
    }
  });

  test("Core child environment strips direct database runtime store variables", () => {
    const previous = {
      BUCEPHALUS_CLOUD_API_URL: process.env.BUCEPHALUS_CLOUD_API_URL,
      BUCEPHALUS_CLOUD_WORKER_TOKEN: process.env.BUCEPHALUS_CLOUD_WORKER_TOKEN,
      BUCEPHALUS_RUNNER_INSTANCE_ID: process.env.BUCEPHALUS_RUNNER_INSTANCE_ID,
      BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON: process.env.BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON,
      BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD: process.env.BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD,
      BUCEPHALUS_SECRET_RESOLVER_GCP_AUTH: process.env.BUCEPHALUS_SECRET_RESOLVER_GCP_AUTH,
      AWS_ACCESS_KEY_ID: process.env.AWS_ACCESS_KEY_ID,
      GOOGLE_APPLICATION_CREDENTIALS: process.env.GOOGLE_APPLICATION_CREDENTIALS,
      DATABASE_URL: process.env.DATABASE_URL,
      BUCEPHALUS_WORKER_DATABASE_URL: process.env.BUCEPHALUS_WORKER_DATABASE_URL,
      BUCEPHALUS_RUN_STORE: process.env.BUCEPHALUS_RUN_STORE,
      BUCEPHALUS_RUN_STORE_URL: process.env.BUCEPHALUS_RUN_STORE_URL,
      BUCEPHALUS_RUN_STORE_SCHEMA: process.env.BUCEPHALUS_RUN_STORE_SCHEMA,
    };
    try {
      process.env.BUCEPHALUS_CLOUD_API_URL = "https://cloud.example";
      process.env.BUCEPHALUS_CLOUD_WORKER_TOKEN = "worker-token";
      process.env.BUCEPHALUS_RUNNER_INSTANCE_ID = "runner-instance-1";
      process.env.BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON = "[\"resolver\"]";
      process.env.BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD = "/usr/bin/gcloud";
      process.env.BUCEPHALUS_SECRET_RESOLVER_GCP_AUTH = "metadata";
      process.env.AWS_ACCESS_KEY_ID = "AKIAEXAMPLE";
      process.env.GOOGLE_APPLICATION_CREDENTIALS = "/var/secrets/google.json";
      process.env.DATABASE_URL = "postgres://example";
      process.env.BUCEPHALUS_WORKER_DATABASE_URL = "postgres://worker";
      process.env.BUCEPHALUS_RUN_STORE = "postgres";
      process.env.BUCEPHALUS_RUN_STORE_URL = "postgres://runtime";
      process.env.BUCEPHALUS_RUN_STORE_SCHEMA = "runtime";

      const env = coreRunnerEnv();

      expect(env.BUCEPHALUS_CLOUD_API_URL).toBeUndefined();
      expect(env.BUCEPHALUS_CLOUD_WORKER_TOKEN).toBeUndefined();
      expect(env.BUCEPHALUS_RUNNER_INSTANCE_ID).toBeUndefined();
      expect(env.BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON).toBeUndefined();
      expect(env.BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD).toBeUndefined();
      expect(env.BUCEPHALUS_SECRET_RESOLVER_GCP_AUTH).toBeUndefined();
      expect(env.AWS_ACCESS_KEY_ID).toBeUndefined();
      expect(env.GOOGLE_APPLICATION_CREDENTIALS).toBeUndefined();
      expect(env.DATABASE_URL).toBeUndefined();
      expect(env.BUCEPHALUS_WORKER_DATABASE_URL).toBeUndefined();
      expect(env.BUCEPHALUS_RUN_STORE).toBeUndefined();
      expect(env.BUCEPHALUS_RUN_STORE_URL).toBeUndefined();
      expect(env.BUCEPHALUS_RUN_STORE_SCHEMA).toBeUndefined();
    } finally {
      restoreEnv(previous);
    }
  });

  test("secret refs require an explicit attempt-scoped resolver", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-secrets-"));
    try {
      await expect(materializeAttemptSecrets(
        { secretResolverCommand: null },
        claimWithSecrets({ OPENAI_API_KEY: "secret-manager/projects/dev/openai" }),
        root,
      )).rejects.toThrow("no attempt-scoped secret resolver is configured");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("secret resolver materializes declared refs under attempt workspace", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-secrets-"));
    try {
      const files = await materializeAttemptSecrets(
        { secretResolverCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/secretResolver.ts")] },
        claimWithSecrets({ OPENAI_API_KEY: "secret-manager/projects/dev/openai" }),
        root,
      );

      expect(Object.keys(files)).toEqual(["OPENAI_API_KEY"]);
      expect(files.OPENAI_API_KEY).toBe(join(root, "secrets", "OPENAI_API_KEY.secret"));
      expect((await stat(files.OPENAI_API_KEY!)).mode & 0o777).toBe(0o444);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});

function runtimeAccessWorkerConfig(
  root: string,
  apiUrl: string,
  overrides: {
    networkPolicyCommand?: string[] | null;
    portForwardCommand?: string[] | null;
    execCommand?: string[] | null;
    capabilities?: { executors: string[]; resources: string[] };
  } = {},
) {
  return {
    apiUrl: apiUrl.replace(/\/+$/, ""),
    workerId: "worker-1",
    runnerPoolId: "pool-1",
    runnerInstanceId: "runner-instance-1",
    leaseSeconds: 30,
    pollMs: 1000,
    heartbeatMs: 1000,
    sweeperMs: 1000,
    dataDir: root,
    coreRunnerCommand: "bucephalus",
    workerToken: "worker-token",
    secretResolverCommand: null,
    networkPolicyCommand: null,
    portForwardCommand: null,
    execCommand: null,
    capabilities: { executors: ["runner-docker"], resources: [] },
    minFreeBytes: 0,
    retainAttemptWorkspaces: false,
    provisionRequestId: null,
    providerInstanceId: null,
    workerImageRef: null,
    liveEvidence: true,
    evidenceIntervalMs: 2000,
    coreTimeoutMs: 15 * 60 * 1000,
    coreCompletionGraceMs: 120_000,
    apiRequestTimeoutMs: 30_000,
    ...overrides,
  };
}

function claimWithSecrets(secretRefs: Record<string, string>) {
  return claim({
    secret_refs: secretRefs,
  });
}

function claimWithNetwork(egressHosts: string[]) {
  return claim({
    run_requirements: {
      executor: "runner-docker",
      requires: ["network_perimeter"],
      image_refs: [],
      network_perimeter: {
        default: "none",
        task_sandbox: "none",
        agent: "none",
        egress_hosts: egressHosts,
      },
    },
  });
}

function claim(overrides: {
  secret_refs?: Record<string, string>;
  run_requirements?: Record<string, unknown>;
} = {}) {
  return {
    run: {
      run_id: "run-1",
      package_digest: "sha256:test",
      env: {},
      secret_refs: overrides.secret_refs ?? {},
      runtime_options: {},
      run_requirements: {
        executor: "runner-docker",
        requires: [],
        image_refs: [],
        ...overrides.run_requirements,
      },
    },
    attempt: {
      attempt_id: "attempt-1",
      attempt_token: "attempt-token",
    },
  };
}

async function writeMinimalSealedPackage(root: string): Promise<{ archivePath: string; packageDigest: string }> {
  const packageDir = join(root, "sealed-package");
  await mkdir(packageDir, { recursive: true });
  const resolvedExperiment = currentResolvedExperiment();
  await writeFile(join(packageDir, "resolved_experiment.json"), JSON.stringify(resolvedExperiment));
  await writeFile(join(packageDir, "staging_manifest.json"), JSON.stringify({
    schema_version: "runtime_path_staging_manifest_v1",
    variants: {
      baseline: [],
    },
  }));

  const checksums = await checksumsForPackage(packageDir);
  const packageDigest = sha256Digest(canonicalJsonStringify(checksums.files));
  await writeFile(join(packageDir, "checksums.json"), JSON.stringify(checksums));
  await writeFile(join(packageDir, "package.lock"), JSON.stringify({
    schema_version: "sealed_package_lock_v1",
    package_digest: packageDigest,
  }));
  await writeFile(join(packageDir, "package_checks.json"), JSON.stringify({
    schema_version: "package_checks_v1",
    package_digest: packageDigest,
    passed: true,
    checks: [],
    summary: {
      checks: 0,
      failed: 0,
      warnings: 0,
    },
  }));
  await writeFile(join(packageDir, "manifest.json"), JSON.stringify({
    schema_version: "sealed_run_package_v2",
    created_at: "2026-06-04T00:00:00Z",
    resolved_experiment: resolvedExperiment,
    checksums_ref: "checksums.json",
    package_checks_ref: "package_checks.json",
    package_digest: packageDigest,
  }));

  const archivePath = join(root, "package.tgz");
  await tar.c({ gzip: true, cwd: packageDir, file: archivePath }, (await readdir(packageDir)).sort());
  return { archivePath, packageDigest };
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

function currentResolvedExperiment(): JsonObject {
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

async function runWorkerHelper(
  args: string[],
  input: JsonObject,
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  return await new Promise((resolvePromise, reject) => {
    const child = spawn(process.execPath, [
      "run",
      join(import.meta.dir, "../src/worker.ts"),
      ...args,
    ], {
      stdio: ["pipe", "pipe", "pipe"],
    });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];

    child.stdout.on("data", (chunk: Buffer) => stdout.push(chunk));
    child.stderr.on("data", (chunk: Buffer) => stderr.push(chunk));
    child.on("error", reject);
    child.on("close", (code) => {
      resolvePromise({
        exitCode: code ?? 1,
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
      });
    });
    child.stdin.end(`${JSON.stringify(input)}\n`);
  });
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
