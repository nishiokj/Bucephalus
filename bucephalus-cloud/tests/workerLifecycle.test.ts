import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import * as tar from "tar";
import {
  applyRuntimeNetworkPolicy,
  collectRuntimeSnapshot,
  coreRunnerEnv,
  discoverCoreRunIdsFromRunRoot,
  loadWorkerConfig,
  materializeAttemptSecrets,
  materializePackage,
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
    });

    expect(config.apiUrl).toBe("https://cloud.example");
    expect(config.runnerPoolId).toBe("pool-1");
    expect(config.capabilities.resources).not.toContain("network_perimeter");
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
