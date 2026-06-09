#!/usr/bin/env bun
import { chmod, mkdir, readdir, readFile, rm, stat, statfs, writeFile } from "node:fs/promises";
import type { Dirent } from "node:fs";
import { randomUUID } from "node:crypto";
import { join, resolve } from "node:path";
import os from "node:os";
import { spawn, type ChildProcess } from "node:child_process";
import { request as httpRequest } from "node:http";
import { setTimeout as sleep } from "node:timers/promises";
import { inspectSealedPackageArchive } from "./imports/sealedPackage";

interface WorkerConfig {
  apiUrl: string;
  workerId: string;
  runnerPoolId: string;
  runnerInstanceId: string | null;
  leaseSeconds: number;
  pollMs: number;
  heartbeatMs: number;
  sweeperMs: number;
  dataDir: string;
  coreRunnerCommand: string;
  workerToken: string;
  secretResolverCommand: string[] | null;
  networkPolicyCommand: string[] | null;
  capabilities: WorkerCapabilities;
  minFreeBytes: number;
  retainAttemptWorkspaces: boolean;
  provisionRequestId: string | null;
  providerInstanceId: string | null;
}

type JsonObject = Record<string, unknown>;

const RUNTIME_SNAPSHOT_EVENT_TYPE = "worker.runtime.snapshot";
const RUNTIME_SNAPSHOT_MAX_TRIALS = 200;
const RUNTIME_SNAPSHOT_MAX_EVENTS_PER_TRIAL = 200;
const RUNTIME_SNAPSHOT_MAX_EVIDENCE_RECORDS = 500;
const RUNTIME_SNAPSHOT_MAX_JSON_BYTES = 2 * 1024 * 1024;
const RUNTIME_SNAPSHOT_MAX_PAYLOAD_BYTES = 4 * 1024 * 1024;
const RUNTIME_SNAPSHOT_PAYLOAD_ENVELOPE_BYTES = 128 * 1024;
const REDACTED_VALUE = "[redacted]";
const DOCKER_SOCKET_PATH = "/var/run/docker.sock";
const DOCKER_API_VERSION = "v1.41";

class WorkerError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "WorkerError";
  }
}

let shuttingDown = false;
let activeChild: ChildProcess | null = null;
let runnerInstancePoisoned = false;

async function main(): Promise<void> {
  const config = loadWorkerConfig();
  const instance = await registerRunnerInstance(config);
  config.runnerInstanceId = instance.runner_instance_id;
  console.log(`runner instance registered: ${config.runnerInstanceId}`);
  try {
    await validateWorkerHost(config);
    await cleanupStartupResidue(config);
  } catch (error) {
    await poisonRunnerInstance(config, "startup_cleanup_failed", {
      error: errorMessage(error),
    }).catch((poisonError) => {
      console.error(`failed to mark runner unhealthy: ${errorMessage(poisonError)}`);
    });
    throw error;
  }
  process.on("SIGINT", () => requestShutdown("SIGINT"));
  process.on("SIGTERM", () => requestShutdown("SIGTERM"));

  const sweeper = runSweeper(config).catch((error) => {
    console.error(`worker sweeper stopped: ${errorMessage(error)}`);
    shuttingDown = true;
  });
  const instanceHeartbeat = runInstanceHeartbeat(config).catch((error) => {
    console.error(`runner instance heartbeat stopped: ${errorMessage(error)}`);
    shuttingDown = true;
  });

  try {
    while (!shuttingDown) {
      const claim = await claimRun(config);
      if (claim.claimed) {
        await executeClaimedRun(config, claim);
        continue;
      }
      await sleep(config.pollMs);
    }
  } finally {
    shuttingDown = true;
    await sweeper.catch(() => undefined);
    await instanceHeartbeat.catch(() => undefined);
    if (config.runnerInstanceId && !runnerInstancePoisoned) {
      await markRunnerInstanceOffline(config, "worker_shutdown").catch((error) => {
        console.error(`failed to mark runner offline: ${errorMessage(error)}`);
      });
    }
  }
}

async function runSweeper(config: WorkerConfig): Promise<void> {
  while (!shuttingDown) {
    await sleep(config.sweeperMs);
    if (shuttingDown) {
      return;
    }
    const result = await cloudFetch(config, "/v1/worker/runs/expire-leases", {
      method: "POST",
      body: {},
    });
    if (isRecord(result) && Array.isArray(result.expired) && result.expired.length > 0) {
      console.log(`worker sweeper expired ${result.expired.length} attempt(s)`);
    }
  }
}

async function runInstanceHeartbeat(config: WorkerConfig): Promise<void> {
  while (!shuttingDown) {
    await sleep(config.heartbeatMs);
    if (shuttingDown) {
      return;
    }
    await heartbeatRunnerInstance(config);
  }
}

async function executeClaimedRun(config: WorkerConfig, claim: RunClaim): Promise<void> {
  const attemptId = claim.attempt.attempt_id;
  const runId = claim.run.run_id;
  const workspaceDir = attemptWorkspaceDir(config, claim);
  console.log(`worker ${config.workerId} claimed run ${runId} attempt ${attemptId}`);

  let heartbeatStop = false;
  const heartbeatLoop = (async () => {
    while (!heartbeatStop && !shuttingDown) {
      await sleep(config.heartbeatMs);
      if (heartbeatStop || shuttingDown) {
        return;
      }
      await heartbeat(config, claim);
    }
  })();

  let materialized: MaterializedPackage | null = null;
  let coreError: unknown = null;
  let cleanupError: unknown = null;

  try {
    try {
      await appendEvent(config, claim, "worker.materializing", {
        package_digest: claim.run.package_digest,
        has_env: Object.keys(claim.run.env).length > 0,
        secret_ref_names: Object.keys(claim.run.secret_refs),
      });
      materialized = await materializePackage(config, claim);
      await appendEvent(config, claim, "worker.materialized", {
        workspace_dir: materialized.workspaceDir,
        package_archive_path: materialized.packageArchivePath,
        extracted_dir: materialized.extractedDir,
        run_root_dir: materialized.runRootDir,
        manifest_experiment_id: stringAt(materialized.manifestJson, "/resolved_experiment/experiment/id"),
      });
      await prePullRunImages(config, claim);
      await applyRuntimeNetworkPolicy(config, claim, materialized);
      await executeCoreRun(config, claim, materialized);
    } catch (error) {
      coreError = error;
    }

    if (materialized) {
      try {
        await uploadRuntimeSnapshots(config, claim, materialized);
      } catch (error) {
        await appendEvent(config, claim, "worker.runtime.snapshot_failed", {
          error: errorMessage(error),
        }).catch((eventError) => {
          console.error(`worker ${config.workerId} failed to append runtime snapshot failure event: ${errorMessage(eventError)}`);
        });
        if (!coreError) {
          coreError = error;
        } else {
          console.error(`worker ${config.workerId} failed to upload runtime snapshot: ${errorMessage(error)}`);
        }
      }
    }

    try {
      await cleanupClaimWorkspace(config, claim, materialized ?? {
        workspaceDir,
        packageArchivePath: join(workspaceDir, "package.tgz"),
        extractedDir: join(workspaceDir, "package"),
        runRootDir: join(workspaceDir, "run-root"),
        manifestJson: {},
        secretFiles: {},
      });
    } catch (error) {
      cleanupError = error;
    }

    if (cleanupError) {
      const message = `runner cleanup failed after run ${runId} attempt ${attemptId}: ${errorMessage(cleanupError)}`;
      await fail(config, claim, message).catch((failError) => {
        console.error(`worker ${config.workerId} failed to mark run failed: ${errorMessage(failError)}`);
      });
      await poisonRunnerInstance(config, "attempt_cleanup_failed", {
        run_id: runId,
        attempt_id: attemptId,
        error: errorMessage(cleanupError),
      }).catch((poisonError) => {
        console.error(`failed to mark runner unhealthy: ${errorMessage(poisonError)}`);
      });
      shuttingDown = true;
    } else if (coreError) {
      await fail(config, claim, errorMessage(coreError)).catch((failError) => {
        console.error(`worker ${config.workerId} failed to mark run failed: ${errorMessage(failError)}`);
      });
    } else {
      await complete(config, claim);
      console.log(`worker ${config.workerId} completed run ${runId}`);
    }
  } finally {
    heartbeatStop = true;
    await heartbeatLoop.catch(() => undefined);
  }
}

async function executeCoreRun(
  config: WorkerConfig,
  claim: RunClaim,
  materialized: MaterializedPackage,
): Promise<void> {
  const command = coreRunnerCommand(config, claim, materialized);
  await appendEvent(config, claim, "worker.core.starting", {
    command: command.redactedArgs,
    workspace_dir: materialized.workspaceDir,
    run_root_dir: materialized.runRootDir,
  });
  const result = await runProcess(command.executable, command.args, {
    cwd: materialized.workspaceDir,
    env: coreRunnerEnv(),
  });
  const eventPayload = {
    exit_code: result.exitCode,
    stdout_tail: tail(result.stdout, 16_000),
    stderr_tail: tail(result.stderr, 16_000),
  };
  if (result.exitCode !== 0) {
    await appendEvent(config, claim, "worker.core.failed", eventPayload);
    throw new WorkerError(
      `Core runner exited with ${result.exitCode}: ${tail(result.stderr || result.stdout, 1000)}`,
    );
  }
  await appendEvent(config, claim, "worker.core.completed", eventPayload);
}

async function uploadRuntimeSnapshots(
  config: WorkerConfig,
  claim: RunClaim,
  materialized: MaterializedPackage,
): Promise<void> {
  const coreRunIds = await discoverCoreRunIdsFromRunRoot(materialized.runRootDir);
  if (coreRunIds.length === 0) {
    throw new WorkerError("Core runner completed without producing a Core run directory");
  }
  for (const coreRunId of coreRunIds) {
    const snapshot = await collectRuntimeSnapshot(materialized.runRootDir, coreRunId);
    await appendEvent(config, claim, RUNTIME_SNAPSHOT_EVENT_TYPE, snapshot);
  }
}

export async function collectRuntimeSnapshot(runRootDir: string, coreRunId: string): Promise<RuntimeSnapshotPayload> {
  assertCoreRunId(coreRunId);
  const runDir = join(runRootDir, coreRunId);
  const runtimeDir = join(runDir, "runtime");
  const budget = new RuntimeSnapshotBudget(
    RUNTIME_SNAPSHOT_MAX_PAYLOAD_BYTES - RUNTIME_SNAPSHOT_PAYLOAD_ENVELOPE_BYTES,
  );
  const runtimeValues: Record<string, JsonObject> = {};
  const omitted: string[] = [];
  const omit = (path: string) => {
    if (budget.tryAdd(path)) {
      omitted.push(path);
    }
  };
  for (const [key, relativePath] of [
    ["run_control_v2", "run_control.json"],
    ["schedule_progress_v2", "schedule_progress.json"],
    ["run_session_state_v1", "run_session_state.json"],
  ] as const) {
    const value = await readBoundedJsonObject(join(runtimeDir, relativePath));
    if (value.status === "read") {
      if (budget.tryAdd(value.object)) {
        runtimeValues[key] = value.object;
      } else {
        omit(`runtime/${relativePath}`);
      }
    } else if (value.status === "omitted") {
      omit(`runtime/${relativePath}`);
    }
  }

  const trialSummaries = await collectTrialSummaries(runDir, budget);
  if (trialSummaries.truncated) {
    omit("trials");
  }
  for (const path of trialSummaries.omitted) {
    omit(path);
  }
  const evidenceRecords = await readBoundedJsonLines(
    join(runDir, "evidence", "evidence_records.jsonl"),
    RUNTIME_SNAPSHOT_MAX_EVIDENCE_RECORDS,
  );
  let evidenceRecordItems: JsonObject[] = [];
  if (evidenceRecords.status === "read") {
    for (const record of evidenceRecords.items) {
      if (!budget.tryAdd(record)) {
        omit("evidence/evidence_records.jsonl");
        break;
      }
      evidenceRecordItems.push(record);
    }
    if (evidenceRecords.truncated) {
      omit("evidence/evidence_records.jsonl");
    }
  } else if (evidenceRecords.status === "omitted") {
    omit("evidence/evidence_records.jsonl");
  }
  return {
    core_run_id: coreRunId,
    run_dir_name: coreRunId,
    runtime_values: runtimeValues,
    trial_summaries: trialSummaries.items,
    evidence_records: evidenceRecordItems,
    omitted,
    snapshot_budget: {
      max_payload_bytes: RUNTIME_SNAPSHOT_MAX_PAYLOAD_BYTES,
      estimated_payload_bytes: budget.usedBytes,
      envelope_reserve_bytes: RUNTIME_SNAPSHOT_PAYLOAD_ENVELOPE_BYTES,
    },
  };
}

async function collectTrialSummaries(runDir: string, budget: RuntimeSnapshotBudget): Promise<{
  items: RuntimeTrialSummaryPayload[];
  truncated: boolean;
  omitted: string[];
}> {
  const trialsDir = join(runDir, "trials");
  let entries: Dirent[];
  try {
    entries = await readdir(trialsDir, { withFileTypes: true });
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") {
      return { items: [], truncated: false, omitted: [] };
    }
    throw error;
  }
  const trialDirs = entries.filter((entry) => entry.isDirectory()).map((entry) => entry.name).sort();
  const items: RuntimeTrialSummaryPayload[] = [];
  const omitted: string[] = [];
  const omit = (path: string) => {
    if (budget.tryAdd(path)) {
      omitted.push(path);
    }
  };
  for (const trialId of trialDirs.slice(0, RUNTIME_SNAPSHOT_MAX_TRIALS)) {
    const value = await readBoundedJsonObject(join(trialsDir, trialId, "summary.json"));
    if (value.status === "read") {
      const item: RuntimeTrialSummaryPayload = {
        trial_id: trialId,
        summary: value.object,
      };
      const contractTrace = await readBoundedJsonObject(join(trialsDir, trialId, "runner", "contract_trace.json"));
      if (contractTrace.status === "read") {
        const candidate = {
          ...item,
          contract_trace: contractTrace.object,
        };
        if (budget.fits(candidate)) {
          item.contract_trace = contractTrace.object;
        } else {
          omit(`trials/${trialId}/runner/contract_trace.json`);
        }
      } else if (contractTrace.status === "omitted") {
        omit(`trials/${trialId}/runner/contract_trace.json`);
      }
      const trialEvents = await readBoundedJsonLines(
        join(trialsDir, trialId, "agent", "events.jsonl"),
        RUNTIME_SNAPSHOT_MAX_EVENTS_PER_TRIAL,
      );
      if (trialEvents.status === "read") {
        const candidate = {
          ...item,
          trial_events: trialEvents.items,
        };
        if (budget.fits(candidate)) {
          item.trial_events = trialEvents.items;
        } else {
          omit(`trials/${trialId}/agent/events.jsonl`);
        }
        if (trialEvents.truncated) {
          omit(`trials/${trialId}/agent/events.jsonl`);
        }
      } else if (trialEvents.status === "omitted") {
        omit(`trials/${trialId}/agent/events.jsonl`);
      }
      if (budget.tryAdd(item)) {
        items.push(item);
      } else {
        omit(`trials/${trialId}/summary.json`);
      }
    } else if (value.status === "omitted") {
      omit(`trials/${trialId}/summary.json`);
    }
  }
  return {
    items,
    truncated: trialDirs.length > RUNTIME_SNAPSHOT_MAX_TRIALS,
    omitted,
  };
}

class RuntimeSnapshotBudget {
  usedBytes = 0;

  constructor(private readonly maxBytes: number) {}

  fits(value: unknown): boolean {
    return this.usedBytes + estimatedJsonBytes(value) <= this.maxBytes;
  }

  tryAdd(value: unknown): boolean {
    const bytes = estimatedJsonBytes(value);
    if (this.usedBytes + bytes > this.maxBytes) {
      return false;
    }
    this.usedBytes += bytes;
    return true;
  }
}

function estimatedJsonBytes(value: unknown): number {
  return Buffer.byteLength(JSON.stringify(value), "utf8");
}

async function readBoundedJsonLines(path: string, maxLines: number): Promise<
  | { status: "missing" }
  | { status: "omitted" }
  | { status: "read"; items: JsonObject[]; truncated: boolean }
> {
  let fileStat;
  try {
    fileStat = await stat(path);
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") {
      return { status: "missing" };
    }
    throw error;
  }
  if (!fileStat.isFile() || fileStat.size > RUNTIME_SNAPSHOT_MAX_JSON_BYTES) {
    return { status: "omitted" };
  }
  const lines = (await readFile(path, "utf8")).split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  const items = lines.slice(0, maxLines).map((line) => {
    try {
      const parsed = JSON.parse(line);
      return isRecord(parsed)
        ? redactSensitiveJsonObject(parsed)
        : { event_type: "trajectory_parse_error", error: "event line is not a JSON object" };
    } catch (error) {
      return {
        event_type: "trajectory_parse_error",
        error: errorMessage(error),
        raw_line: line,
      };
    }
  });
  return {
    status: "read",
    items,
    truncated: lines.length > maxLines,
  };
}

async function readBoundedJsonObject(path: string): Promise<
  | { status: "missing" }
  | { status: "omitted" }
  | { status: "read"; object: JsonObject }
> {
  let fileStat;
  try {
    fileStat = await stat(path);
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") {
      return { status: "missing" };
    }
    throw error;
  }
  if (!fileStat.isFile() || fileStat.size > RUNTIME_SNAPSHOT_MAX_JSON_BYTES) {
    return { status: "omitted" };
  }
  const parsed = JSON.parse(await readFile(path, "utf8"));
  if (!isRecord(parsed)) {
    return { status: "omitted" };
  }
  return { status: "read", object: redactSensitiveJsonObject(parsed) };
}

function assertCoreRunId(coreRunId: string): void {
  if (!/^run_[A-Za-z0-9_.-]+$/.test(coreRunId)) {
    throw new WorkerError(`Invalid Core run id '${coreRunId}'`);
  }
}

function redactSensitiveJsonObject(value: JsonObject): JsonObject {
  return redactSensitiveJsonValue(value, null) as JsonObject;
}

function redactSensitiveJsonValue(value: unknown, key: string | null): unknown {
  if (key !== null && sensitiveJsonKey(key)) {
    return REDACTED_VALUE;
  }
  if (typeof value === "string") {
    return sensitiveStringValue(value) ? REDACTED_VALUE : value;
  }
  if (Array.isArray(value)) {
    return value.map((item) => redactSensitiveJsonValue(item, null));
  }
  if (isRecord(value)) {
    const out: JsonObject = {};
    for (const [childKey, childValue] of Object.entries(value)) {
      out[childKey] = redactSensitiveJsonValue(childValue, childKey);
    }
    return out;
  }
  return value;
}

function sensitiveJsonKey(key: string): boolean {
  const normalized = key.toLowerCase().replaceAll(/[^a-z0-9]/g, "");
  return normalized.includes("secret")
    || normalized.includes("password")
    || normalized.includes("passwd")
    || normalized.includes("token")
    || normalized.includes("apikey")
    || normalized.includes("credential")
    || normalized.includes("authorization")
    || normalized.includes("bearer");
}

function sensitiveStringValue(value: string): boolean {
  return value.includes("gcp-secret-manager://")
    || value.includes("aws-secrets-manager://")
    || /\bAKIA[0-9A-Z]{16}\b/.test(value)
    || /\bASIA[0-9A-Z]{16}\b/.test(value)
    || /\bsk-[A-Za-z0-9_-]{20,}\b/.test(value)
    || /\bya29\.[A-Za-z0-9_-]{20,}\b/.test(value);
}

export function coreRunnerEnv(): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {
    ...process.env,
  };
  delete env.BUCEPHALUS_CLOUD_API_URL;
  delete env.BUCEPHALUS_CLOUD_WORKER_TOKEN;
  delete env.BUCEPHALUS_CLOUD_ALLOW_CONTROL_PLANE_SECRET_REFS;
  delete env.BUCEPHALUS_CLOUD_ALLOW_LOCAL_IMAGE_REFS;
  delete env.BUCEPHALUS_RUNNER_POOL_ID;
  delete env.BUCEPHALUS_RUNNER_INSTANCE_ID;
  delete env.BUCEPHALUS_RUNNER_PROVIDER_INSTANCE_ID;
  delete env.BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID;
  delete env.DATABASE_URL;
  delete env.BUCEPHALUS_WORKER_DATABASE_URL;
  delete env.BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON;
  delete env.BUCEPHALUS_RUN_STORE;
  delete env.BUCEPHALUS_RUN_STORE_URL;
  delete env.BUCEPHALUS_RUN_STORE_SCHEMA;
  delete env.BUCEPHALUS_SECRET_RESOLVER_AWS_CMD;
  delete env.BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD;
  delete env.BUCEPHALUS_SECRET_RESOLVER_GCP_AUTH;
  delete env.BUCEPHALUS_SECRET_RESOLVER_ALLOW_CONTROL_PLANE_REFS;
  delete env.BUCEPHALUS_SECRET_RESOLVER_ALLOW_ENV;
  delete env.AWS_ACCESS_KEY_ID;
  delete env.AWS_SECRET_ACCESS_KEY;
  delete env.AWS_SESSION_TOKEN;
  delete env.GOOGLE_APPLICATION_CREDENTIALS;
  return env;
}

function coreRunnerCommand(
  config: WorkerConfig,
  claim: RunClaim,
  materialized: MaterializedPackage,
): { executable: string; args: string[]; redactedArgs: string[] } {
  const args = [
    "run",
    materialized.extractedDir,
    "--json",
    "--run-root",
    materialized.runRootDir,
  ];
  const runtimeOptions = claim.run.runtime_options;
  const smokeTest = runtimeOptions.smoke_test === true;
  if (smokeTest) {
    args.push("--smoke-test");
  } else {
    args.push("--run-dangerously");
  }

  const executor = optionalRuntimeString(runtimeOptions.executor);
  const cloudExecutor = claim.run.run_requirements.executor;
  const coreExecutor = executor ?? coreExecutorForCloudExecutor(cloudExecutor);
  if (coreExecutor) {
    args.push("--executor", coreExecutor);
  }
  const materialize = optionalRuntimeString(runtimeOptions.materialize);
  if (materialize) {
    args.push("--materialize", materialize);
  }

  for (const [key, value] of Object.entries(claim.run.env)) {
    assertRuntimeEnvKey(key);
    args.push("--env", `${key}=${value}`);
  }

  const redactedArgs = [...args];
  for (const [id, secretPath] of Object.entries(materialized.secretFiles)) {
    args.push("--secret-file", `${id}=${secretPath}`);
    redactedArgs.push("--secret-file", `${id}=<secret:${claim.run.secret_refs[id] ?? "redacted"}>`);
  }

  return {
    executable: config.coreRunnerCommand,
    args,
    redactedArgs,
  };
}

function coreExecutorForCloudExecutor(executor: string): string | null {
  switch (executor) {
    case "runner-docker":
      return "local_docker";
    case "modal":
      return "modal";
    default:
      throw new WorkerError(`Unsupported Cloud runner executor '${executor}'`);
  }
}

function optionalRuntimeString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function assertRuntimeEnvKey(key: string): void {
  if (!/^[A-Z_][A-Z0-9_]*$/.test(key)) {
    throw new WorkerError(`Invalid runtime env key '${key}'`);
  }
}

function assertSecretId(id: string): void {
  if (!/^[A-Za-z0-9_.-]+$/.test(id)) {
    throw new WorkerError(`Invalid secret id '${id}'`);
  }
}

async function runProcess(
  executable: string,
  args: string[],
  options: { cwd: string; env: NodeJS.ProcessEnv },
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  return await new Promise((resolve, reject) => {
    const child = spawn(executable, args, {
      cwd: options.cwd,
      env: options.env,
      detached: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
    activeChild = child;
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    child.stdout.on("data", (chunk: Buffer) => stdout.push(chunk));
    child.stderr.on("data", (chunk: Buffer) => stderr.push(chunk));
    child.on("error", (error) => {
      if (activeChild === child) {
        activeChild = null;
      }
      reject(error);
    });
    child.on("close", (code) => {
      if (activeChild === child) {
        activeChild = null;
      }
      resolve({
        exitCode: code ?? 1,
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
      });
    });
  });
}

async function claimRun(config: WorkerConfig): Promise<RunClaim | EmptyClaim> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  return await cloudFetch(config, "/v1/worker/runs/claim", {
    method: "POST",
    body: {
      runner_instance_id: runnerInstanceId,
      lease_seconds: config.leaseSeconds,
    },
  }) as RunClaim | EmptyClaim;
}

async function heartbeat(config: WorkerConfig, claim: RunClaim): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/worker/run-attempts/${claim.attempt.attempt_id}/heartbeat`, {
    method: "POST",
    authToken: claim.attempt.attempt_token,
    body: {
      runner_instance_id: runnerInstanceId,
      lease_seconds: config.leaseSeconds,
    },
  });
}

async function registerRunnerInstance(config: WorkerConfig): Promise<RunnerInstance> {
  return await cloudFetch(config, "/v1/runner-instances/register", {
    method: "POST",
    body: {
      runner_pool_id: config.runnerPoolId,
      instance_name: config.workerId,
      capabilities: config.capabilities,
      metadata: await runnerMetadata(config),
    },
  }) as RunnerInstance;
}

async function heartbeatRunnerInstance(config: WorkerConfig): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/runner-instances/${runnerInstanceId}/heartbeat`, {
    method: "POST",
    body: {
      capabilities: config.capabilities,
      metadata: await runnerMetadata(config),
    },
  });
}

async function poisonRunnerInstance(
  config: WorkerConfig,
  reason: string,
  details: JsonObject,
): Promise<void> {
  runnerInstancePoisoned = true;
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/runner-instances/${runnerInstanceId}/unhealthy`, {
    method: "POST",
    body: {
      reason,
      details,
    },
  });
}

async function markRunnerInstanceOffline(config: WorkerConfig, reason: string): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/runner-instances/${runnerInstanceId}/offline`, {
    method: "POST",
    body: { reason },
  });
}

export async function materializePackage(
  config: WorkerConfig,
  claim: RunClaim,
): Promise<MaterializedPackage> {
  const workspaceDir = attemptWorkspaceDir(config, claim);
  const packageArchivePath = join(workspaceDir, "package.tgz");
  const extractedDir = join(workspaceDir, "package");
  const runRootDir = join(workspaceDir, "run-root");
  await rm(workspaceDir, { recursive: true, force: true });
  await mkdir(extractedDir, { recursive: true });
  await mkdir(runRootDir, { recursive: true });
  const packageBytes = await cloudFetchBytes(config, `/v1/packages/${encodeURIComponent(claim.run.package_digest)}/content`, {
    authToken: claim.attempt.attempt_token,
    attemptId: claim.attempt.attempt_id,
  });
  await writeFile(packageArchivePath, packageBytes);
  const inspection = await inspectSealedPackageArchive({
    archivePath: packageArchivePath,
    workDir: extractedDir,
  });
  if (inspection.packageDigest !== claim.run.package_digest) {
    throw new WorkerError(
      `Downloaded package digest mismatch: claim expected ${claim.run.package_digest}, package declares ${inspection.packageDigest ?? "<missing>"}`,
    );
  }

  await writeFile(
    join(workspaceDir, "run-env.json"),
    `${JSON.stringify({
      env: claim.run.env,
      secret_refs: claim.run.secret_refs,
      runtime_options: claim.run.runtime_options,
    }, null, 2)}\n`,
  );
  const secretFiles = await materializeAttemptSecrets(config, claim, workspaceDir);

  return {
    workspaceDir,
    packageArchivePath,
    extractedDir,
    runRootDir,
    manifestJson: inspection.manifestJson,
    secretFiles,
  };
}

export async function materializeAttemptSecrets(
  config: Pick<WorkerConfig, "secretResolverCommand">,
  claim: Pick<RunClaim, "run" | "attempt">,
  workspaceDir: string,
): Promise<Record<string, string>> {
  const secretEntries = Object.entries(claim.run.secret_refs);
  if (secretEntries.length === 0) {
    return {};
  }
  for (const [id, ref] of secretEntries) {
    assertSecretId(id);
    assertSecretRef(ref);
  }
  if (!config.secretResolverCommand) {
    throw new WorkerError(
      "Run declares secret_refs, but no attempt-scoped secret resolver is configured. "
        + "Set BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON to a provider-managed resolver; "
        + "local persistent secret directories are not a Cloud runtime boundary.",
    );
  }

  const secretDir = join(workspaceDir, "secrets");
  await mkdir(secretDir, { recursive: true, mode: 0o700 });
  const result = await runJsonCommand(config.secretResolverCommand, {
    attempt_id: claim.attempt.attempt_id,
    run_id: claim.run.run_id,
    output_dir: secretDir,
    secrets: secretEntries.map(([id, ref]) => ({ id, ref })),
  });
  if (!isRecord(result) || !isRecord(result.files)) {
    throw new WorkerError("Secret resolver must return a JSON object with a files object");
  }

  const files: Record<string, string> = {};
  for (const [id, value] of Object.entries(result.files)) {
    assertSecretId(id);
    if (typeof value !== "string" || value.trim().length === 0) {
      throw new WorkerError(`Secret resolver returned an invalid file for '${id}'`);
    }
    if (!Object.prototype.hasOwnProperty.call(claim.run.secret_refs, id)) {
      throw new WorkerError(`Secret resolver returned undeclared secret id '${id}'`);
    }
    const outputPath = resolvedSecretOutputPath(secretDir, value);
    const fileStat = await stat(outputPath);
    if (!fileStat.isFile()) {
      throw new WorkerError(`Secret resolver output for '${id}' is not a file`);
    }
    await chmod(outputPath, 0o600);
    files[id] = outputPath;
  }
  const missing = secretEntries.map(([id]) => id).filter((id) => !files[id]);
  if (missing.length > 0) {
    throw new WorkerError(`Secret resolver did not materialize required secret id(s): ${missing.join(", ")}`);
  }
  return files;
}

export async function applyRuntimeNetworkPolicy(
  config: Pick<WorkerConfig, "networkPolicyCommand" | "workerId" | "runnerInstanceId">,
  claim: Pick<RunClaim, "run" | "attempt">,
  materialized: Pick<MaterializedPackage, "workspaceDir" | "runRootDir">,
): Promise<void> {
  const networkPerimeter = runtimeNetworkPerimeter(claim.run.run_requirements);
  if (networkPerimeter.egress_hosts.length === 0) {
    return;
  }
  if (!config.networkPolicyCommand) {
    throw new WorkerError(
      "Run declares runtime network egress requirements, but no network policy enforcer is configured. "
        + "Set BUCEPHALUS_WORKER_NETWORK_POLICY_CMD_JSON to a provider-managed enforcer; "
        + "ambient VM network access is not a Cloud runtime boundary.",
    );
  }
  await runJsonCommand(config.networkPolicyCommand, {
    attempt_id: claim.attempt.attempt_id,
    run_id: claim.run.run_id,
    runner_instance_id: config.runnerInstanceId,
    worker_id: config.workerId,
    workspace_dir: materialized.workspaceDir,
    run_root_dir: materialized.runRootDir,
    network_perimeter: networkPerimeter,
    egress_hosts: networkPerimeter.egress_hosts,
  });
}

export async function prePullRunImages(
  config: Pick<WorkerConfig, "capabilities">,
  claim: Pick<RunClaim, "run" | "attempt">,
): Promise<void> {
  if (!config.capabilities.resources.includes("docker_daemon")
    || !config.capabilities.resources.includes("registry_pull")) {
    return;
  }
  const imageRefs = Array.isArray(claim.run.run_requirements.image_refs)
    ? claim.run.run_requirements.image_refs.filter((item): item is string => typeof item === "string")
    : [];
  for (const imageRef of [...new Set(imageRefs)]) {
    await dockerPullImage(imageRef);
  }
}

function runtimeNetworkPerimeter(requirements: RunRequirements): RuntimeNetworkPerimeter {
  const raw = requirements.network_perimeter;
  if (!isRecord(raw)) {
    return {
      default: "none",
      task_sandbox: "none",
      agent: "none",
      egress_hosts: [],
    };
  }
  const defaultMode = runtimeNetworkMode(raw.default);
  return {
    default: defaultMode,
    task_sandbox: runtimeNetworkMode(raw.task_sandbox ?? defaultMode),
    agent: runtimeNetworkMode(raw.agent ?? defaultMode),
    egress_hosts: Array.isArray(raw.egress_hosts)
      ? raw.egress_hosts.filter((item): item is string => typeof item === "string")
      : [],
  };
}

function runtimeNetworkMode(value: unknown): RuntimeNetworkMode {
  return value === "allowlist_enforced" ? "allowlist_enforced" : "none";
}

function assertSecretRef(ref: string): void {
  if (typeof ref !== "string" || ref.trim().length === 0) {
    throw new WorkerError("Secret ref must be a non-empty string");
  }
  if (ref.includes("\n") || ref.includes("\r")) {
    throw new WorkerError(`Invalid secret ref '${ref}'`);
  }
}

function resolvedSecretOutputPath(secretDir: string, resolverPath: string): string {
  if (resolverPath.startsWith("/")) {
    throw new WorkerError("Secret resolver returned an absolute path; expected a path relative to output_dir");
  }
  const outputPath = resolve(secretDir, resolverPath);
  const outputRoot = resolve(secretDir);
  if (outputPath !== outputRoot && !outputPath.startsWith(`${outputRoot}/`)) {
    throw new WorkerError("Secret resolver returned a path outside output_dir");
  }
  return outputPath;
}

function attemptWorkspaceDir(config: WorkerConfig, claim: RunClaim): string {
  return join(config.dataDir, "worker-runs", claim.run.run_id, claim.attempt.attempt_id);
}

async function cleanupClaimWorkspace(
  config: WorkerConfig,
  claim: RunClaim,
  materialized: MaterializedPackage,
): Promise<void> {
  await appendEvent(config, claim, "worker.cleanup.starting", {
    workspace_dir: materialized.workspaceDir,
    run_root_dir: materialized.runRootDir,
    retain_attempt_workspace: config.retainAttemptWorkspaces,
  }).catch((error) => {
    console.error(`worker ${config.workerId} failed to append cleanup start event: ${errorMessage(error)}`);
  });

  const cleanup = await cleanupAttemptWorkspace(config, materialized);

  await appendEvent(config, claim, "worker.cleanup.completed", {
    workspace_dir: materialized.workspaceDir,
    core_run_ids: cleanup.coreRunIds,
    docker_resources_removed: cleanup.dockerResourcesRemoved,
    workspace_removed: cleanup.workspaceRemoved,
  }).catch((error) => {
    console.error(`worker ${config.workerId} failed to append cleanup completed event: ${errorMessage(error)}`);
  });
}

async function cleanupAttemptWorkspace(
  config: WorkerConfig,
  materialized: MaterializedPackage,
): Promise<AttemptCleanupResult> {
  const coreRunIds = await discoverCoreRunIdsFromRunRoot(materialized.runRootDir);
  const dockerResourcesRemoved = await cleanupDockerRuntimeResources(config, coreRunIds);
  let workspaceRemoved = false;
  if (!config.retainAttemptWorkspaces) {
    await rm(materialized.workspaceDir, { recursive: true, force: true });
    workspaceRemoved = true;
  }
  return {
    coreRunIds,
    dockerResourcesRemoved,
    workspaceRemoved,
  };
}

export async function discoverCoreRunIdsFromRunRoot(runRootDir: string): Promise<string[]> {
  let entries: Dirent[];
  try {
    entries = await readdir(runRootDir, { withFileTypes: true });
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") {
      return [];
    }
    throw error;
  }
  return entries
    .filter((entry) => entry.isDirectory() && entry.name.startsWith("run_"))
    .map((entry) => entry.name)
    .sort();
}

async function cleanupDockerRuntimeResources(
  config: WorkerConfig,
  coreRunIds: string[],
): Promise<DockerCleanupSummary> {
  if (!config.capabilities.resources.includes("docker_daemon")) {
    return { containers: 0, networks: 0, volumes: 0 };
  }
  const summary: DockerCleanupSummary = { containers: 0, networks: 0, volumes: 0 };
  for (const coreRunId of coreRunIds) {
    const labels = [`label=bucephalus.run_id=${coreRunId}`];
    summary.containers += await removeDockerResources("container", labels);
    summary.networks += await removeDockerResources("network", labels);
    summary.volumes += await removeDockerResources("volume", labels);
  }
  return summary;
}

async function cleanupStartupResidue(config: WorkerConfig): Promise<void> {
  const workerRunsDir = join(config.dataDir, "worker-runs");
  await mkdir(workerRunsDir, { recursive: true });
  if (config.capabilities.resources.includes("docker_daemon")) {
    await cleanupAllBucephalusDockerResources();
  }

  const entries = await readdir(workerRunsDir, { withFileTypes: true });
  for (const runEntry of entries) {
    if (!runEntry.isDirectory()) {
      continue;
    }
    const runDir = join(workerRunsDir, runEntry.name);
    const attemptEntries = await readdir(runDir, { withFileTypes: true }).catch((error) => {
      if (isNodeError(error) && error.code === "ENOENT") {
        return [];
      }
      throw error;
    });
    for (const attemptEntry of attemptEntries) {
      if (!attemptEntry.isDirectory()) {
        continue;
      }
      const workspaceDir = join(runDir, attemptEntry.name);
      const runRootDir = join(workspaceDir, "run-root");
      const coreRunIds = await discoverCoreRunIdsFromRunRoot(runRootDir);
      await cleanupDockerRuntimeResources(config, coreRunIds);
      await rm(workspaceDir, { recursive: true, force: true });
    }
    await rm(runDir, { recursive: true, force: true });
  }
}

async function cleanupAllBucephalusDockerResources(): Promise<DockerCleanupSummary> {
  const labels = ["label=bucephalus.run_id"];
  return {
    containers: await removeDockerResources("container", labels),
    networks: await removeDockerResources("network", labels),
    volumes: await removeDockerResources("volume", labels),
  };
}

async function removeDockerResources(
  kind: "container" | "network" | "volume",
  filters: string[],
): Promise<number> {
  const ids = await listDockerResourceIds(kind, filters);
  if (ids.length === 0) {
    return 0;
  }
  for (const id of ids) {
    await removeDockerResource(kind, id);
  }
  return ids.length;
}

async function listDockerResourceIds(
  kind: "container" | "network" | "volume",
  filters: string[],
): Promise<string[]> {
  const query = `filters=${encodeURIComponent(JSON.stringify(dockerLabelFilters(filters)))}`;
  if (kind === "container") {
    const containers = await dockerRequest<Array<{ Id?: unknown }>>("GET", `/containers/json?all=1&${query}`);
    return containers.map((item) => typeof item.Id === "string" ? item.Id : "").filter(Boolean);
  }
  if (kind === "network") {
    const networks = await dockerRequest<Array<{ Id?: unknown }>>("GET", `/networks?${query}`);
    return networks.map((item) => typeof item.Id === "string" ? item.Id : "").filter(Boolean);
  }
  const volumes = await dockerRequest<{ Volumes?: Array<{ Name?: unknown }> }>("GET", `/volumes?${query}`);
  return (volumes.Volumes ?? []).map((item) => typeof item.Name === "string" ? item.Name : "").filter(Boolean);
}

async function removeDockerResource(kind: "container" | "network" | "volume", id: string): Promise<void> {
  const encoded = encodeURIComponent(id);
  if (kind === "container") {
    await dockerRequest("DELETE", `/containers/${encoded}?force=true`);
  } else if (kind === "network") {
    await dockerRequest("DELETE", `/networks/${encoded}`);
  } else {
    await dockerRequest("DELETE", `/volumes/${encoded}`);
  }
}

function dockerLabelFilters(filters: string[]): { label: string[] } {
  return {
    label: filters.map((filter) => filter.startsWith("label=") ? filter.slice("label=".length) : filter),
  };
}

async function dockerPullImage(imageRef: string): Promise<void> {
  const response = await dockerRequestText(
    "POST",
    `/images/create?fromImage=${encodeURIComponent(imageRef)}`,
    {
      headers: await dockerRegistryAuthHeaders(imageRef),
    },
  );
  const errors: string[] = [];
  for (const line of response.body.split(/\r?\n/).map((item) => item.trim()).filter(Boolean)) {
    try {
      const parsed = JSON.parse(line);
      if (isRecord(parsed) && typeof parsed.error === "string" && parsed.error.trim().length > 0) {
        errors.push(parsed.error);
      }
    } catch {
      // Docker streams JSON objects; ignore non-JSON progress fragments defensively.
    }
  }
  if (errors.length > 0) {
    throw new WorkerError(`Docker image pull failed for ${imageRef}: ${tail(errors.join("\n"), 1000)}`);
  }
}

export async function dockerRegistryAuthHeaders(imageRef: string): Promise<Record<string, string>> {
  const registry = registryHostFromImageRef(imageRef);
  const auth = await dockerRegistryAuth(registry);
  if (!auth) {
    return {};
  }
  return {
    "X-Registry-Auth": Buffer.from(JSON.stringify(auth)).toString("base64url"),
  };
}

async function dockerRegistryAuth(registry: string): Promise<JsonObject | null> {
  const dockerConfigDir = process.env.DOCKER_CONFIG
    ?? (process.env.HOME ? join(process.env.HOME, ".docker") : null);
  if (!dockerConfigDir) {
    return null;
  }
  try {
    const parsed = JSON.parse(await readFile(join(dockerConfigDir, "config.json"), "utf8"));
    if (!isRecord(parsed) || !isRecord(parsed.auths)) {
      return null;
    }
    const direct = parsed.auths[registry];
    const https = parsed.auths[`https://${registry}`];
    const entry = isRecord(direct) ? direct : isRecord(https) ? https : null;
    if (!entry) {
      return null;
    }
    const decodedAuth = dockerAuthCredential(entry);
    const username = typeof entry.username === "string" ? entry.username : decodedAuth?.username;
    const password = typeof entry.password === "string" ? entry.password : decodedAuth?.password;
    return {
      username,
      password,
      serveraddress: registry,
    };
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") {
      return null;
    }
    throw error;
  }
}

function dockerAuthCredential(entry: JsonObject): { username: string; password: string } | null {
  if (typeof entry.auth !== "string" || entry.auth.trim().length === 0) {
    return null;
  }
  let decoded: string;
  try {
    decoded = Buffer.from(entry.auth, "base64").toString("utf8");
  } catch {
    return null;
  }
  const separator = decoded.indexOf(":");
  if (separator <= 0) {
    return null;
  }
  return {
    username: decoded.slice(0, separator),
    password: decoded.slice(separator + 1),
  };
}

function registryHostFromImageRef(imageRef: string): string {
  const name = imageRef.split("@", 1)[0]?.split(":", 1)[0] ?? imageRef;
  const first = name.split("/", 1)[0] ?? "";
  if (first.includes(".") || first.includes(":") || first === "localhost") {
    return first;
  }
  return "index.docker.io";
}

async function dockerRequest<T = unknown>(method: "GET" | "DELETE", apiPath: string): Promise<T> {
  const { body } = await dockerRequestText(method, apiPath);
  if (body.trim() === "") {
    return undefined as T;
  }
  return JSON.parse(body) as T;
}

async function dockerRequestText(
  method: "GET" | "DELETE" | "POST",
  apiPath: string,
  options: { headers?: Record<string, string> } = {},
): Promise<{ statusCode: number; body: string }> {
  const path = `/${DOCKER_API_VERSION}${apiPath}`;
  const { statusCode, body } = await new Promise<{ statusCode: number; body: string }>((resolve, reject) => {
    const request = httpRequest({
      socketPath: DOCKER_SOCKET_PATH,
      path,
      method,
      headers: options.headers,
    }, (response) => {
      const chunks: Buffer[] = [];
      response.on("data", (chunk: Buffer) => chunks.push(chunk));
      response.on("end", () => {
        resolve({
          statusCode: response.statusCode ?? 0,
          body: Buffer.concat(chunks).toString("utf8"),
        });
      });
    });
    request.on("error", reject);
    request.end();
  });
  if (statusCode < 200 || statusCode >= 300) {
    throw new WorkerError(`Docker API ${method} ${apiPath} returned ${statusCode}: ${tail(body, 1000)}`);
  }
  return { statusCode, body };
}

async function validateWorkerHost(config: WorkerConfig): Promise<void> {
  await mkdir(config.dataDir, { recursive: true });
  const resources = await workerResourceSnapshot(config);
  if (resources.data_dir_free_bytes < config.minFreeBytes) {
    throw new WorkerError(
      `runner data dir free bytes below floor: required=${config.minFreeBytes} available=${resources.data_dir_free_bytes}`,
    );
  }
  if (config.capabilities.resources.includes("docker_daemon")) {
    await dockerRequest("GET", "/version");
  }
}

async function runnerMetadata(config: WorkerConfig): Promise<JsonObject> {
  const resources = await workerResourceSnapshot(config).catch((error) => ({
    error: errorMessage(error),
  }));
  return {
    daemon: "bucephalus-cloud-worker",
    ...(config.provisionRequestId ? { provision_request_id: config.provisionRequestId } : {}),
    ...(config.providerInstanceId ? { provider_instance_id: config.providerInstanceId } : {}),
    cleanup_policy: {
      mode: "reuse_vm_mandatory_cleanup_poison_on_failure",
      retain_attempt_workspaces: config.retainAttemptWorkspaces,
    },
    resources,
  };
}

async function workerResourceSnapshot(config: WorkerConfig): Promise<JsonObject & { data_dir_free_bytes: number }> {
  await mkdir(config.dataDir, { recursive: true });
  const fsStats = await statfs(config.dataDir);
  const freeBytes = Number(fsStats.bavail) * Number(fsStats.bsize);
  return {
    cpu_count: os.cpus().length,
    total_memory_bytes: os.totalmem(),
    free_memory_bytes: os.freemem(),
    data_dir: config.dataDir,
    data_dir_free_bytes: freeBytes,
    min_free_bytes: config.minFreeBytes,
  };
}

async function appendEvent(
  config: WorkerConfig,
  claim: RunClaim,
  eventType: string,
  payload: JsonObject,
): Promise<void> {
  await cloudFetch(config, `/v1/worker/run-attempts/${claim.attempt.attempt_id}/events`, {
    method: "POST",
    authToken: claim.attempt.attempt_token,
    body: {
      runner_instance_id: requireRunnerInstanceId(config),
      event_type: eventType,
      payload,
    },
  });
}

async function complete(config: WorkerConfig, claim: RunClaim): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/worker/run-attempts/${claim.attempt.attempt_id}/complete`, {
    method: "POST",
    authToken: claim.attempt.attempt_token,
    body: {
      runner_instance_id: runnerInstanceId,
    },
  });
}

async function fail(config: WorkerConfig, claim: RunClaim, message: string): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/worker/run-attempts/${claim.attempt.attempt_id}/fail`, {
    method: "POST",
    authToken: claim.attempt.attempt_token,
    body: {
      runner_instance_id: runnerInstanceId,
      message,
    },
  });
}

async function cloudFetch(
  config: WorkerConfig,
  path: string,
  options: { method?: string; body?: unknown; authToken?: string } = {},
): Promise<unknown> {
  const init: RequestInit = {
    method: options.method ?? "GET",
    headers: workerAuthHeaders(config, options.authToken),
  };
  if (options.body !== undefined) {
    init.headers = { ...workerAuthHeaders(config, options.authToken), "content-type": "application/json" };
    init.body = JSON.stringify(options.body);
  }
  const response = await fetch(`${config.apiUrl}${path}`, init);
  const text = await response.text();
  const payload = text.trim().length > 0 ? JSON.parse(text) : null;
  if (!response.ok) {
    throw new WorkerError(
      isRecord(payload) && typeof payload.message === "string"
        ? payload.message
        : `Cloud API request failed: ${response.status}`,
    );
  }
  return payload;
}

async function cloudFetchBytes(
  config: WorkerConfig,
  path: string,
  options: { authToken?: string; attemptId?: string } = {},
): Promise<Uint8Array> {
  const response = await fetch(`${config.apiUrl}${path}`, {
    headers: {
      ...workerAuthHeaders(config, options.authToken),
      ...(options.attemptId ? { "x-bucephalus-attempt-id": options.attemptId } : {}),
    },
  });
  if (!response.ok) {
    const text = await response.text();
    let message = `Cloud API request failed: ${response.status}`;
    try {
      const payload = text.trim().length > 0 ? JSON.parse(text) : null;
      if (isRecord(payload) && typeof payload.message === "string") {
        message = payload.message;
      }
    } catch {
      if (text.trim().length > 0) {
        message = text;
      }
    }
    throw new WorkerError(message);
  }
  return new Uint8Array(await response.arrayBuffer());
}

export function loadWorkerConfig(env: NodeJS.ProcessEnv = process.env): WorkerConfig {
  const apiUrl = requiredEnv(env.BUCEPHALUS_CLOUD_API_URL, "BUCEPHALUS_CLOUD_API_URL");
  const leaseSeconds = numberEnv(env.BUCEPHALUS_WORKER_LEASE_SECONDS, 30);
  return {
    apiUrl: apiUrl.replace(/\/+$/, ""),
    workerId: env.BUCEPHALUS_WORKER_ID ?? `worker-${randomUUID()}`,
    runnerPoolId: requiredEnv(env.BUCEPHALUS_RUNNER_POOL_ID, "BUCEPHALUS_RUNNER_POOL_ID"),
    runnerInstanceId: null,
    leaseSeconds,
    pollMs: numberEnv(env.BUCEPHALUS_WORKER_POLL_MS, 2000),
    heartbeatMs: numberEnv(env.BUCEPHALUS_WORKER_HEARTBEAT_MS, Math.max(1000, Math.floor((leaseSeconds * 1000) / 3))),
    sweeperMs: numberEnv(env.BUCEPHALUS_WORKER_SWEEPER_MS, 5000),
    dataDir: resolve(env.BUCEPHALUS_CLOUD_DATA_DIR ?? ".data"),
    coreRunnerCommand: env.BUCEPHALUS_CORE_RUNNER_CMD ?? "bucephalus",
    workerToken: requiredEnv(env.BUCEPHALUS_CLOUD_WORKER_TOKEN, "BUCEPHALUS_CLOUD_WORKER_TOKEN"),
    secretResolverCommand: optionalCommandJson(
      env.BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON,
      "BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON",
    ),
    networkPolicyCommand: optionalCommandJson(
      env.BUCEPHALUS_WORKER_NETWORK_POLICY_CMD_JSON,
      "BUCEPHALUS_WORKER_NETWORK_POLICY_CMD_JSON",
    ),
    capabilities: workerCapabilities(env),
    minFreeBytes: numberEnv(
      env.BUCEPHALUS_WORKER_MIN_FREE_BYTES ?? env.BUCEPHALUS_MIN_FREE_BYTES,
      20 * 1024 * 1024 * 1024,
    ),
    retainAttemptWorkspaces: booleanEnv(env.BUCEPHALUS_WORKER_RETAIN_ATTEMPT_WORKSPACES, false),
    provisionRequestId: env.BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID?.trim() || null,
    providerInstanceId: env.BUCEPHALUS_RUNNER_PROVIDER_INSTANCE_ID?.trim() || null,
  };
}

function workerAuthHeaders(config: WorkerConfig, token = config.workerToken): Record<string, string> {
  return {
    authorization: `Bearer ${token}`,
  };
}

function optionalCommandJson(raw: string | undefined, name: string): string[] | null {
  if (!raw || raw.trim().length === 0) {
    return null;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new WorkerError(`invalid ${name}: ${errorMessage(error)}`);
  }
  if (!Array.isArray(parsed) || parsed.length === 0 || parsed.some((item) => typeof item !== "string" || item.trim().length === 0)) {
    throw new WorkerError(`${name} must be a non-empty JSON string array`);
  }
  return parsed.map((item) => item.trim());
}

async function runJsonCommand(command: string[], input: JsonObject): Promise<unknown> {
  const [executable, ...args] = command;
  if (!executable) {
    throw new WorkerError("command is empty");
  }
  const result = await new Promise<{ exitCode: number; stdout: string; stderr: string }>((resolvePromise, reject) => {
    const child = spawn(executable, args, {
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
  if (result.exitCode !== 0) {
    throw new WorkerError(`${executable} exited ${result.exitCode}: ${tail(result.stderr || result.stdout, 1000)}`);
  }
  try {
    return JSON.parse(result.stdout);
  } catch (error) {
    throw new WorkerError(`command returned invalid JSON: ${errorMessage(error)}`);
  }
}

function requestShutdown(signal: NodeJS.Signals): void {
  shuttingDown = true;
  const child = activeChild;
  if (!child?.pid) {
    return;
  }
  signalChildProcessGroup(child, signal);
  const killTimer = globalThis.setTimeout(() => {
    if (activeChild === child) {
      signalChildProcessGroup(child, "SIGKILL");
    }
  }, 5000);
  killTimer.unref?.();
}

function signalChildProcessGroup(child: ChildProcess, signal: NodeJS.Signals): void {
  if (!child.pid) {
    return;
  }
  try {
    process.kill(-child.pid, signal);
  } catch {
    child.kill(signal);
  }
}

function requireRunnerInstanceId(config: WorkerConfig): string {
  if (!config.runnerInstanceId) {
    throw new WorkerError("Runner instance is not registered");
  }
  return config.runnerInstanceId;
}

function requiredEnv(value: string | undefined, name: string): string {
  if (!value || value.trim().length === 0) {
    throw new WorkerError(`${name} is required`);
  }
  return value.trim();
}

function workerCapabilities(env: NodeJS.ProcessEnv): WorkerCapabilities {
  const resources = csvEnv(env.BUCEPHALUS_WORKER_RESOURCES, ["core_runner", "docker_daemon", "registry_pull"]);
  if (env.BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON && !resources.includes("secret_resolver")) {
    resources.push("secret_resolver");
  }
  if (env.BUCEPHALUS_WORKER_NETWORK_POLICY_CMD_JSON && !resources.includes("network_perimeter")) {
    resources.push("network_perimeter");
  }
  return {
    executors: csvEnv(env.BUCEPHALUS_WORKER_EXECUTORS, ["runner-docker"]),
    resources,
    arch: normalizeArch(env.BUCEPHALUS_WORKER_ARCH ?? os.arch()),
    cpu_count: numberEnv(env.BUCEPHALUS_WORKER_CPU_COUNT, os.cpus().length),
    memory_mb: numberEnv(env.BUCEPHALUS_WORKER_MEMORY_MB, Math.floor(os.totalmem() / 1024 / 1024)),
    disk_mb: numberEnv(env.BUCEPHALUS_WORKER_DISK_MB, Math.floor(numberEnv(env.BUCEPHALUS_WORKER_MIN_FREE_BYTES ?? env.BUCEPHALUS_MIN_FREE_BYTES, 20 * 1024 * 1024 * 1024) / 1024 / 1024)),
    isolation: csvEnv(env.BUCEPHALUS_WORKER_ISOLATION, ["reusable_vm"]),
  };
}

function normalizeArch(value: string): string {
  switch (value.trim().toLowerCase()) {
    case "x64":
    case "amd64":
    case "x86_64":
      return "x86_64";
    case "arm64":
    case "aarch64":
      return "arm64";
    default:
      return value.trim();
  }
}

function csvEnv(value: string | undefined, fallback: string[]): string[] {
  if (!value) {
    return fallback;
  }
  return value
    .split(",")
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
}

function tail(value: string, maxBytes: number): string {
  const buffer = Buffer.from(value, "utf8");
  if (buffer.byteLength <= maxBytes) {
    return value;
  }
  return buffer.subarray(buffer.byteLength - maxBytes).toString("utf8");
}

function numberEnv(value: string | undefined, fallback: number): number {
  if (!value) {
    return fallback;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function booleanEnv(value: string | undefined, fallback: boolean): boolean {
  if (value === undefined) {
    return fallback;
  }
  const normalized = value.trim().toLowerCase();
  if (["1", "true", "yes", "on"].includes(normalized)) {
    return true;
  }
  if (["0", "false", "no", "off"].includes(normalized)) {
    return false;
  }
  return fallback;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isNodeError(error: unknown): error is NodeJS.ErrnoException {
  return error instanceof Error && "code" in error;
}

interface EmptyClaim {
  claimed: false;
}

interface RunClaim {
  claimed: true;
  run: {
    run_id: string;
    package_digest: string;
    env: Record<string, string>;
    secret_refs: Record<string, string>;
    runtime_options: JsonObject;
    run_requirements: RunRequirements;
  };
  attempt: {
    attempt_id: string;
    attempt_token: string;
  };
}

interface RunnerInstance {
  runner_instance_id: string;
}

interface RunRequirements {
  executor: string;
  requires: string[];
  image_refs: string[];
  secret_ids?: string[];
  network_perimeter?: JsonObject;
  sidecars?: string[];
  accelerators?: string[];
  arch?: string;
  cpu_count?: number;
  memory_mb?: number;
  disk_mb?: number;
  isolation?: string;
  timeout_ms?: number | null;
  max_parallel_trials?: number;
}

type RuntimeNetworkMode = "none" | "allowlist_enforced";

interface RuntimeNetworkPerimeter extends JsonObject {
  default: RuntimeNetworkMode;
  task_sandbox: RuntimeNetworkMode;
  agent: RuntimeNetworkMode;
  egress_hosts: string[];
}

interface WorkerCapabilities {
  executors: string[];
  resources: string[];
  arch?: string;
  cpu_count?: number;
  memory_mb?: number;
  disk_mb?: number;
  isolation?: string[];
}

interface MaterializedPackage {
  workspaceDir: string;
  packageArchivePath: string;
  extractedDir: string;
  runRootDir: string;
  manifestJson: JsonObject;
  secretFiles: Record<string, string>;
}

interface DockerCleanupSummary {
  containers: number;
  networks: number;
  volumes: number;
}

interface AttemptCleanupResult {
  coreRunIds: string[];
  dockerResourcesRemoved: DockerCleanupSummary;
  workspaceRemoved: boolean;
}

interface RuntimeSnapshotPayload extends JsonObject {
  core_run_id: string;
  run_dir_name: string;
  runtime_values: Record<string, JsonObject>;
  trial_summaries: RuntimeTrialSummaryPayload[];
  evidence_records: JsonObject[];
  omitted: string[];
  snapshot_budget: JsonObject;
}

interface RuntimeTrialSummaryPayload extends JsonObject {
  trial_id: string;
  summary: JsonObject;
  contract_trace?: JsonObject;
  trial_events?: JsonObject[];
}

function stringAt(root: JsonObject, pointer: string): string | null {
  let current: unknown = root;
  for (const rawSegment of pointer.split("/").slice(1)) {
    const segment = rawSegment.replaceAll("~1", "/").replaceAll("~0", "~");
    if (!isRecord(current)) {
      return null;
    }
    current = current[segment];
  }
  return typeof current === "string" ? current : null;
}

if (import.meta.main) {
  main().catch((error) => {
    console.error(errorMessage(error));
    process.exit(1);
  });
}
