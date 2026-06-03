import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "bun:test";
import {
  applyRuntimeNetworkPolicy,
  collectRuntimeSnapshot,
  coreRunnerEnv,
  discoverCoreRunIdsFromRunRoot,
  loadWorkerConfig,
  materializeAttemptSecrets,
} from "../src/worker";

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
      DATABASE_URL: process.env.DATABASE_URL,
      BUCEPHALUS_WORKER_DATABASE_URL: process.env.BUCEPHALUS_WORKER_DATABASE_URL,
      BUCEPHALUS_RUN_STORE: process.env.BUCEPHALUS_RUN_STORE,
      BUCEPHALUS_RUN_STORE_URL: process.env.BUCEPHALUS_RUN_STORE_URL,
      BUCEPHALUS_RUN_STORE_SCHEMA: process.env.BUCEPHALUS_RUN_STORE_SCHEMA,
    };
    try {
      process.env.DATABASE_URL = "postgres://example";
      process.env.BUCEPHALUS_WORKER_DATABASE_URL = "postgres://worker";
      process.env.BUCEPHALUS_RUN_STORE = "postgres";
      process.env.BUCEPHALUS_RUN_STORE_URL = "postgres://runtime";
      process.env.BUCEPHALUS_RUN_STORE_SCHEMA = "runtime";

      const env = coreRunnerEnv();

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
    },
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
