import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import * as tar from "tar";
import {
  applyRuntimeNetworkPolicy,
  collectRuntimeSnapshot,
  coreRunnerFailureMessage,
  coreRunnerCommand,
  coreRunnerEnv,
  discoverCoreRunIdsFromRunRoot,
  loadWorkerConfig,
  materializeAttemptSecrets,
  materializePackage,
  materializedPackageEventPayload,
  redactedProcessTail,
  redactedWorkerErrorMessage,
  runnerMetadata,
} from "../src/worker";
import { canonicalJsonStringify, sha256Digest, type JsonObject } from "../src/primitives";

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

  test("omits malformed runtime JSON without dropping the whole snapshot", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-runtime-malformed-"));
    try {
      const runRoot = join(root, "run-root");
      const coreRunId = "run_20260529_000001_000001_000001";
      const runtimeDir = join(runRoot, coreRunId, "runtime");
      const badTrialDir = join(runRoot, coreRunId, "trials", "trial-bad");
      const goodTrialDir = join(runRoot, coreRunId, "trials", "trial-good");
      await mkdir(runtimeDir, { recursive: true });
      await mkdir(badTrialDir, { recursive: true });
      await mkdir(goodTrialDir, { recursive: true });
      await writeFile(join(runtimeDir, "run_control.json"), "{not-json");
      await writeFile(join(runtimeDir, "schedule_progress.json"), JSON.stringify({ committed: 1, total: 2 }));
      await writeFile(join(badTrialDir, "summary.json"), "{not-json");
      await writeFile(join(goodTrialDir, "summary.json"), JSON.stringify({ outcome: "success" }));

      const snapshot = await collectRuntimeSnapshot(runRoot, coreRunId);
      const text = JSON.stringify(snapshot);

      expect(snapshot.runtime_values.run_control_v2).toBeUndefined();
      expect(snapshot.runtime_values.schedule_progress_v2).toEqual({ committed: 1, total: 2 });
      expect(snapshot.trial_summaries).toEqual([
        {
          trial_id: "trial-good",
          summary: { outcome: "success" },
        },
      ]);
      expect(snapshot.omitted).toContain("runtime/run_control.json");
      expect(snapshot.omitted).toContain("trials/trial-bad/summary.json");
      expect(text).not.toContain("{not-json");
      expect(text).not.toContain(root);
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
      await writeFile(join(runtimeDir, "run_session_state.json"), JSON.stringify({
        schema_version: "run_session_state_v1",
        run_id: coreRunId,
        project_root: root,
        execution: {
          executor: "local-docker",
          run_root_path: join(root, "run-root"),
        },
      }));
      await writeFile(join(trialDir, "summary.json"), JSON.stringify({
        outcome: "success",
        workspace: root,
        metrics: {
          value: 1,
          api_key: "sk-secretsecretsecretsecretsecret",
        },
      }));
      await writeFile(join(trialDir, "agent", "events.jsonl"), [
        JSON.stringify({
          event_type: "step",
          message: "safe message",
          secret_ref: "gcp-secret-manager://projects/acme/secrets/openai/versions/1",
          path: join(root, "agent", "events.jsonl"),
        }),
        `not-json secret=sk-secretsecretsecretsecretsecret path=${root}`,
        "",
      ].join("\n"));

      const snapshot = await collectRuntimeSnapshot(runRoot, coreRunId);
      const text = JSON.stringify(snapshot);

      expect(snapshot.runtime_values.run_control_v2?.access_token).toBe("[redacted]");
      expect(snapshot.runtime_values.run_session_state_v1?.project_root).toBe("[redacted]");
      expect((snapshot.runtime_values.run_session_state_v1?.execution as JsonObject).run_root_path).toBe("[redacted]");
      expect(snapshot.trial_summaries[0]?.summary.metrics).toMatchObject({
        value: 1,
        api_key: "[redacted]",
      });
      expect(snapshot.trial_summaries[0]?.summary.workspace).toBe("[redacted]");
      expect(snapshot.trial_summaries[0]?.trial_events?.[0]).toMatchObject({
        event_type: "step",
        message: "safe message",
        secret_ref: "[redacted]",
        path: "[redacted]",
      });
      expect(snapshot.trial_summaries[0]?.trial_events?.[1]).toMatchObject({
        event_type: "trajectory_parse_error",
        error: "event line is not valid JSON",
        raw_line_omitted: true,
      });
      expect(text).not.toContain("worker-token-value");
      expect(text).not.toContain("sk-secret");
      expect(text).not.toContain("gcp-secret-manager://");
      expect(text).not.toContain(root);
      expect(text).not.toContain("not-json");
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
    });

    expect(config.apiUrl).toBe("https://cloud.example");
    expect(config.runnerPoolId).toBe("pool-1");
    expect(config.capabilities.resources).not.toContain("network_perimeter");
  });

  test("runner config normalizes advertised executor aliases", () => {
    const config = loadWorkerConfig({
      BUCEPHALUS_CLOUD_API_URL: "https://cloud.example",
      BUCEPHALUS_CLOUD_WORKER_TOKEN: "worker-token",
      BUCEPHALUS_RUNNER_POOL_ID: "pool-1",
      BUCEPHALUS_WORKER_EXECUTORS: "runner_docker,local-docker,modal",
      BUCEPHALUS_WORKER_MIN_FREE_BYTES: "1",
    });

    expect(config.capabilities.executors).toEqual(["modal", "runner-docker"]);
    expect(() => loadWorkerConfig({
      BUCEPHALUS_CLOUD_API_URL: "https://cloud.example",
      BUCEPHALUS_CLOUD_WORKER_TOKEN: "worker-token",
      BUCEPHALUS_RUNNER_POOL_ID: "pool-1",
      BUCEPHALUS_WORKER_EXECUTORS: "kubernetes",
      BUCEPHALUS_WORKER_MIN_FREE_BYTES: "1",
    })).toThrow("Unsupported runner executor 'kubernetes'");
  });

  test("runner config requires an explicit Cloud API URL", () => {
    expect(() => loadWorkerConfig({
      BUCEPHALUS_CLOUD_WORKER_TOKEN: "worker-token",
      BUCEPHALUS_RUNNER_POOL_ID: "pool-1",
      BUCEPHALUS_WORKER_MIN_FREE_BYTES: "1",
    })).toThrow("BUCEPHALUS_CLOUD_API_URL is required");
  });

  test("runner metadata reports capacity without host data directory paths", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-metadata-"));
    try {
      const config = loadWorkerConfig({
        BUCEPHALUS_CLOUD_API_URL: "https://cloud.example",
        BUCEPHALUS_CLOUD_WORKER_TOKEN: "worker-token",
        BUCEPHALUS_RUNNER_POOL_ID: "pool-1",
        BUCEPHALUS_CLOUD_DATA_DIR: root,
        BUCEPHALUS_WORKER_MIN_FREE_BYTES: "1",
      });

      const metadata = await runnerMetadata(config);
      const text = JSON.stringify(metadata);

      expect((metadata.resources as JsonObject).data_dir).toBe("worker_data_dir");
      expect((metadata.resources as JsonObject).data_dir_free_bytes).toBeGreaterThan(0);
      expect(text).not.toContain(root);
      expect(text).not.toContain("/var/folders");
    } finally {
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
          capabilities: { executors: [], resources: [] },
          minFreeBytes: 0,
          retainAttemptWorkspaces: false,
          provisionRequestId: null,
          providerInstanceId: null,
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

  test("network policy application rejects malformed persisted Cloud perimeter requirements", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-network-"));
    try {
      for (const [run_requirements, message] of [
        [
          {
            executor: "runner-docker",
            requires: ["network_perimeter"],
            image_refs: [],
          },
          "/run_requirements/network_perimeter is required",
        ],
        [
          {
            executor: "runner-docker",
            requires: ["network_perimeter"],
            image_refs: [],
            network_perimeter: "allowlist",
          },
          "/run_requirements/network_perimeter must be an object",
        ],
        [
          {
            executor: "runner-docker",
            requires: ["network_perimeter"],
            image_refs: [],
            network_perimeter: {
              default: "full",
              egress_hosts: ["api.openai.com"],
            },
          },
          "/run_requirements/network_perimeter/default must be 'none' or 'allowlist_enforced'",
        ],
        [
          {
            executor: "runner-docker",
            requires: ["network_perimeter"],
            image_refs: [],
            network_perimeter: {
              default: "allowlist_enforced",
              egress_hosts: "api.openai.com",
            },
          },
          "/run_requirements/network_perimeter/egress_hosts must be an array",
        ],
        [
          {
            executor: "runner-docker",
            requires: ["network_perimeter"],
            image_refs: [],
            network_perimeter: {
              default: "allowlist_enforced",
              egress_hosts: ["api.openai.com", ""],
            },
          },
          "/run_requirements/network_perimeter/egress_hosts/1 must be a non-empty string",
        ],
        [
          {
            executor: "runner-docker",
            requires: ["network_perimeter"],
            image_refs: [],
            network_perimeter: {
              default: "allowlist_enforced",
              egress_hosts: [],
            },
          },
          "allowlist_enforced network modes require egress hosts",
        ],
      ] as const) {
        await expect(applyRuntimeNetworkPolicy(
          {
            networkPolicyCommand: null,
            workerId: "worker-1",
            runnerInstanceId: "runner-instance-1",
          },
          claim({ run_requirements }),
          {
            workspaceDir: root,
            runRootDir: join(root, "run-root"),
          },
        )).rejects.toThrow(message);
      }
    } finally {
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

  test("secret refs reject malformed persisted maps before resolver setup", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-secrets-malformed-"));
    try {
      for (const [secret_refs, message] of [
        [null, "/secret_refs must be an object"],
        ["OPENAI_API_KEY=ref", "/secret_refs must be an object"],
        [["OPENAI_API_KEY=ref"], "/secret_refs must be an object"],
        [{ OPENAI_API_KEY: 7 }, "/secret_refs/OPENAI_API_KEY must be a string"],
      ] as const) {
        await expect(materializeAttemptSecrets(
          { secretResolverCommand: null },
          claim({ secret_refs }),
          root,
        )).rejects.toThrow(message);
      }
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
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("secret resolver helper failures omit stdout and stderr from worker errors", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-secret-fail-"));
    try {
      await expect(materializeAttemptSecrets(
        { secretResolverCommand: [process.execPath, "run", join(import.meta.dir, "fixtures/failingSecretResolver.ts")] },
        claimWithSecrets({
          OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/1",
        }),
        root,
      )).rejects.toThrow("configured worker helper command exited 42; stdout/stderr omitted");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("worker materialized event payload does not expose local workspace paths or secret refs", () => {
    const payload = materializedPackageEventPayload({
      manifestJson: {
        resolved_experiment: {
          experiment: {
            id: "exp-1",
          },
        },
      },
      secretFiles: {
        OPENAI_API_KEY: "/tmp/attempt/secrets/OPENAI_API_KEY.secret",
      },
    });
    const text = JSON.stringify(payload);

    expect(payload).toMatchObject({
      workspace: "attempt_workspace",
      package_archive: "package.tgz",
      extracted_package: "package",
      run_root: "run-root",
      manifest_experiment_id: "exp-1",
      secret_file_count: 1,
    });
    expect(text).not.toContain("/tmp/attempt");
    expect(text).not.toContain("gcp-secret-manager://");
  });

  test("Core command event args redact workspace paths, env values, and secret refs", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-command-"));
    try {
      const materialized = {
        workspaceDir: root,
        packageArchivePath: join(root, "package.tgz"),
        extractedDir: join(root, "package"),
        runRootDir: join(root, "run-root"),
        manifestJson: {},
        secretFiles: {
          OPENAI_API_KEY: join(root, "secrets", "OPENAI_API_KEY.secret"),
        },
      };
      const command = coreRunnerCommand(
        {
          coreRunnerCommand: "bucephalus",
        } as never,
        {
          claimed: true,
          run: {
            ...claimWithSecrets({
              OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/1",
            }).run,
            env: {
              PUBLIC_FLAG: "user-visible-value",
            },
            runtime_options: {
              materialize: "metadata-only",
            },
          },
          attempt: {
            attempt_id: "attempt-1",
            attempt_token: "attempt-token",
          },
        },
        materialized,
      );

      expect(command.args).toContain(join(root, "package"));
      expect(command.args).toContain("PUBLIC_FLAG=user-visible-value");
      expect(command.args).toContain(`OPENAI_API_KEY=${join(root, "secrets", "OPENAI_API_KEY.secret")}`);
      const redacted = JSON.stringify(command.redactedArgs);
      expect(redacted).not.toContain(root);
      expect(redacted).not.toContain("user-visible-value");
      expect(redacted).not.toContain("gcp-secret-manager://");
      expect(redacted).not.toContain("OPENAI_API_KEY.secret");
      expect(command.redactedArgs).toContain("PUBLIC_FLAG=<env>");
      expect(command.redactedArgs).toContain("OPENAI_API_KEY=<secret-file>");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("Core command normalizes Cloud executor aliases and materialize modes", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-command-normalize-"));
    try {
      const materialized = {
        workspaceDir: root,
        packageArchivePath: join(root, "package.tgz"),
        extractedDir: join(root, "package"),
        runRootDir: join(root, "run-root"),
        manifestJson: {},
        secretFiles: {},
      };
      const command = coreRunnerCommand(
        {
          coreRunnerCommand: "bucephalus",
        } as never,
        claim({
          runtime_options: {
            executor: "runner-docker",
            materialize: "metadata-only",
          },
          run_requirements: {
            executor: "runner-docker",
          },
        }),
        materialized,
      );

      expect(command.args).toContain("--executor");
      expect(command.args).toContain("local_docker");
      expect(command.args).not.toContain("runner-docker");
      expect(command.args).toContain("--materialize");
      expect(command.args).toContain("metadata_only");
      expect(command.args).not.toContain("metadata-only");
      expect(command.redactedArgs).toContain("local_docker");
      expect(command.redactedArgs).toContain("metadata_only");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("Core command rejects invalid Cloud executor and materialize spellings before launch", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-command-invalid-"));
    try {
      const materialized = {
        workspaceDir: root,
        packageArchivePath: join(root, "package.tgz"),
        extractedDir: join(root, "package"),
        runRootDir: join(root, "run-root"),
        manifestJson: {},
        secretFiles: {},
      };

      expect(() => coreRunnerCommand(
        {
          coreRunnerCommand: "bucephalus",
        } as never,
        claim({
          runtime_options: {
            executor: "docker",
          },
        }),
        materialized,
      )).toThrow("Unsupported Cloud/Core runner executor");

      expect(() => coreRunnerCommand(
        {
          coreRunnerCommand: "bucephalus",
        } as never,
        claim({
          runtime_options: {
            materialize: "metadata",
          },
        }),
        materialized,
      )).toThrow("Unsupported Core materialize mode");

      expect(() => coreRunnerCommand(
        {
          coreRunnerCommand: "bucephalus",
        } as never,
        claim({
          runtime_options: {
            backend: "modal",
            executor: "runner-docker",
          },
          run_requirements: {
            executor: "modal",
          },
        }),
        materialized,
      )).toThrow("runtime_options.backend and runtime_options.executor");

      expect(() => coreRunnerCommand(
        {
          coreRunnerCommand: "bucephalus",
        } as never,
        claim({
          runtime_options: {
            executor: "modal",
          },
          run_requirements: {
            executor: "runner-docker",
          },
        }),
        materialized,
      )).toThrow("runtime_options executor does not match the queued Cloud runner executor");

      expect(() => coreRunnerCommand(
        {
          coreRunnerCommand: "bucephalus",
        } as never,
        claim({
          runtime_options: {
            backend: "runner-docker",
          },
          run_requirements: {
            executor: "modal",
          },
        }),
        materialized,
      )).toThrow("runtime_options executor does not match the queued Cloud runner executor");

      for (const runtime_options of [null, ["executor=modal"], "executor=modal"]) {
        expect(() => coreRunnerCommand(
          {
            coreRunnerCommand: "bucephalus",
          } as never,
          claim({
            runtime_options: runtime_options as never,
          }),
          materialized,
        )).toThrow("/runtime_options must be an object");
      }

      for (const [env, message] of [
        [null, "/env must be an object"],
        ["MODEL=gpt-4.1", "/env must be an object"],
        [["MODEL=gpt-4.1"], "/env must be an object"],
        [{ MODEL: 7 }, "/env/MODEL must be a string"],
      ] as const) {
        expect(() => coreRunnerCommand(
          {
            coreRunnerCommand: "bucephalus",
          } as never,
          claim({ env }),
          materialized,
        )).toThrow(message);
      }
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("Core process output tails redact local paths, env values, and secret refs", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-output-"));
    try {
      const materialized = {
        workspaceDir: root,
        packageArchivePath: join(root, "package.tgz"),
        extractedDir: join(root, "package"),
        runRootDir: join(root, "run-root"),
        secretFiles: {
          OPENAI_API_KEY: join(root, "secrets", "OPENAI_API_KEY.secret"),
        },
      };
      const claimForOutput = {
        run: {
          ...claimWithSecrets({
            OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/1",
          }).run,
          env: {
            PUBLIC_FLAG: "user-visible-value",
          },
        },
        attempt: {
          attempt_token: "attempt-token-value",
        },
      };
      const redacted = redactedProcessTail(
        [
          `workspace=${root}`,
          `package=${join(root, "package")}`,
          `secret_path=${join(root, "secrets", "OPENAI_API_KEY.secret")}`,
          "ref=gcp-secret-manager://projects/acme/secrets/openai/versions/1",
          "env=user-visible-value",
          "attempt=attempt-token-value",
          "token=sk-abcdefghijklmnopqrstuvwx",
        ].join("\n"),
        materialized,
        claimForOutput,
      );

      expect(redacted).toContain("<attempt-workspace>");
      expect(redacted).toContain("<package-dir>");
      expect(redacted).toContain("<secret-file:OPENAI_API_KEY>");
      expect(redacted).toContain("<secret-ref:OPENAI_API_KEY>");
      expect(redacted).toContain("<env:PUBLIC_FLAG>");
      expect(redacted).toContain("<attempt-token>");
      expect(redacted).toContain("[redacted]");
      expect(redacted).not.toContain(root);
      expect(redacted).not.toContain("user-visible-value");
      expect(redacted).not.toContain("gcp-secret-manager://");
      expect(redacted).not.toContain("attempt-token-value");
      expect(redacted).not.toContain("sk-abcdefghijklmnopqrstuvwx");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("Core failure messages redact output before durable run error storage", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-failure-"));
    try {
      const materialized = {
        workspaceDir: root,
        packageArchivePath: join(root, "package.tgz"),
        extractedDir: join(root, "package"),
        runRootDir: join(root, "run-root"),
        secretFiles: {
          OPENAI_API_KEY: join(root, "secrets", "OPENAI_API_KEY.secret"),
        },
      };
      const claimForFailure = {
        run: {
          ...claimWithSecrets({
            OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/1",
          }).run,
          env: {
            PUBLIC_FLAG: "user-visible-value",
          },
        },
        attempt: {
          attempt_token: "attempt-token-value",
        },
      };

      const message = coreRunnerFailureMessage(
        2,
        "",
        [
          `workspace=${root}`,
          `run_root=${join(root, "run-root")}`,
          `secret_path=${join(root, "secrets", "OPENAI_API_KEY.secret")}`,
          "ref=gcp-secret-manager://projects/acme/secrets/openai/versions/1",
          "env=user-visible-value",
          "attempt=attempt-token-value",
        ].join("\n"),
        materialized,
        claimForFailure,
      );

      expect(message).toContain("Core runner exited with 2");
      expect(message).toContain("<attempt-workspace>");
      expect(message).toContain("<run-root>");
      expect(message).toContain("<secret-file:OPENAI_API_KEY>");
      expect(message).toContain("<secret-ref:OPENAI_API_KEY>");
      expect(message).toContain("<env:PUBLIC_FLAG>");
      expect(message).toContain("<attempt-token>");
      expect(message).not.toContain(root);
      expect(message).not.toContain("OPENAI_API_KEY.secret");
      expect(message).not.toContain("gcp-secret-manager://");
      expect(message).not.toContain("user-visible-value");
      expect(message).not.toContain("attempt-token-value");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("worker error messages redact attempt paths and secret material", async () => {
    const root = await mkdtemp(join(tmpdir(), "buc-worker-error-"));
    try {
      const materialized = {
        workspaceDir: root,
        packageArchivePath: join(root, "package.tgz"),
        extractedDir: join(root, "package"),
        runRootDir: join(root, "run-root"),
        secretFiles: {
          OPENAI_API_KEY: join(root, "secrets", "OPENAI_API_KEY.secret"),
        },
      };
      const claimForError = {
        run: {
          ...claimWithSecrets({
            OPENAI_API_KEY: "gcp-secret-manager://projects/acme/secrets/openai/versions/1",
          }).run,
          env: {
            PUBLIC_FLAG: "user-visible-value",
          },
        },
        attempt: {
          attempt_token: "attempt-token-value",
        },
      };

      const message = redactedWorkerErrorMessage(
        new Error([
          `cleanup failed for ${join(root, "run-root")}`,
          `secret=${join(root, "secrets", "OPENAI_API_KEY.secret")}`,
          "ref=gcp-secret-manager://projects/acme/secrets/openai/versions/1",
          "env=user-visible-value",
          "attempt=attempt-token-value",
        ].join(" ")),
        materialized,
        claimForError,
      );

      expect(message).toContain("<run-root>");
      expect(message).toContain("<secret-file:OPENAI_API_KEY>");
      expect(message).toContain("<secret-ref:OPENAI_API_KEY>");
      expect(message).toContain("<env:PUBLIC_FLAG>");
      expect(message).toContain("<attempt-token>");
      expect(message).not.toContain(root);
      expect(message).not.toContain("gcp-secret-manager://");
      expect(message).not.toContain("user-visible-value");
      expect(message).not.toContain("attempt-token-value");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});

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
  env?: unknown;
  secret_refs?: unknown;
  runtime_options?: unknown;
  run_requirements?: Record<string, unknown>;
} = {}) {
  return {
    claimed: true as const,
    run: {
      run_id: "run-1",
      package_digest: "sha256:test",
      env: Object.prototype.hasOwnProperty.call(overrides, "env")
        ? overrides.env as Record<string, string>
        : {},
      secret_refs: Object.prototype.hasOwnProperty.call(overrides, "secret_refs")
        ? overrides.secret_refs as Record<string, string>
        : {},
      runtime_options: Object.prototype.hasOwnProperty.call(overrides, "runtime_options")
        ? overrides.runtime_options as Record<string, unknown>
        : {},
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
    schema_version: "package_staging_manifest_v1",
  }));

  const checksums = await checksumsForPackage(packageDir);
  const packageDigest = sha256Digest(canonicalJsonStringify(checksums.files));
  await writeFile(join(packageDir, "checksums.json"), JSON.stringify(checksums));
  await writeFile(join(packageDir, "package.lock"), JSON.stringify({
    schema_version: "sealed_package_lock_v1",
    package_digest: packageDigest,
  }));
  await writeFile(join(packageDir, "manifest.json"), JSON.stringify({
    schema_version: "sealed_run_package_v2",
    created_at: "2026-06-04T00:00:00Z",
    resolved_experiment: resolvedExperiment,
    checksums_ref: "checksums.json",
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

function restoreEnv(previous: Record<string, string | undefined>): void {
  for (const [key, value] of Object.entries(previous)) {
    if (value === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }
}
