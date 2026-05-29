#!/usr/bin/env bun
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { join } from "node:path";
import { spawn, type ChildProcess } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import * as tar from "tar";
import { loadConfig } from "./config";
import { createSql } from "./db/client";

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
  secretDir: string | null;
  capabilities: WorkerCapabilities;
}

type JsonObject = Record<string, unknown>;

class WorkerError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "WorkerError";
  }
}

let shuttingDown = false;
let wakeRequested = false;
let activeChild: ChildProcess | null = null;

async function main(): Promise<void> {
  const config = loadWorkerConfig();
  const instance = await registerRunnerInstance(config);
  config.runnerInstanceId = instance.runner_instance_id;
  console.log(`runner instance registered: ${config.runnerInstanceId}`);
  const sql = createSql();
  const unlisten = await sql.listen(
    "cloud_runs_available",
    (runId) => {
      wakeRequested = true;
      console.log(`worker wake: run available ${runId}`);
    },
    () => {
      console.log(`worker ${config.workerId} listening for cloud_runs_available`);
    },
  );

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
      wakeRequested = false;
      const claim = await claimRun(config);
      if (claim.claimed) {
        await executeClaimedRun(config, claim);
        continue;
      }
      await sleep(config.pollMs);
      if (wakeRequested) {
        continue;
      }
    }
  } finally {
    shuttingDown = true;
    await unlisten.unlisten().catch(() => undefined);
    await sql.end({ timeout: 1 });
    await sweeper.catch(() => undefined);
    await instanceHeartbeat.catch(() => undefined);
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
  console.log(`worker ${config.workerId} claimed run ${runId} attempt ${attemptId}`);

  let heartbeatStop = false;
  const heartbeatLoop = (async () => {
    while (!heartbeatStop && !shuttingDown) {
      await sleep(config.heartbeatMs);
      if (heartbeatStop || shuttingDown) {
        return;
      }
      await heartbeat(config, attemptId);
    }
  })();

  try {
    await appendEvent(config, claim, "worker.materializing", {
      package_digest: claim.run.package_digest,
      has_env: Object.keys(claim.run.env).length > 0,
      secret_ref_names: Object.keys(claim.run.secret_refs),
    });
    const materialized = await materializePackage(config, claim);
    await appendEvent(config, claim, "worker.materialized", {
      workspace_dir: materialized.workspaceDir,
      package_archive_path: materialized.packageArchivePath,
      extracted_dir: materialized.extractedDir,
      run_root_dir: materialized.runRootDir,
      manifest_experiment_id: stringAt(materialized.manifestJson, "/resolved_experiment/experiment/id"),
    });
    await executeCoreRun(config, claim, materialized);
    await complete(config, attemptId);
    console.log(`worker ${config.workerId} completed run ${runId}`);
  } catch (error) {
    await fail(config, attemptId, errorMessage(error)).catch((failError) => {
      console.error(`worker ${config.workerId} failed to mark run failed: ${errorMessage(failError)}`);
    });
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
    env: process.env,
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
  for (const [id, ref] of Object.entries(claim.run.secret_refs)) {
    assertSecretId(id);
    const secretPath = resolveSecretRef(config, ref);
    args.push("--secret-file", `${id}=${secretPath}`);
    redactedArgs.push("--secret-file", `${id}=<secret:${ref}>`);
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

function resolveSecretRef(config: WorkerConfig, ref: string): string {
  if (!config.secretDir) {
    throw new WorkerError(
      `Run provided secret ref '${ref}', but BUCEPHALUS_WORKER_SECRET_DIR is not configured`,
    );
  }
  if (!/^[A-Za-z0-9_.-]+$/.test(ref)) {
    throw new WorkerError(`Invalid secret ref '${ref}'`);
  }
  return join(config.secretDir, ref);
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

async function heartbeat(config: WorkerConfig, attemptId: string): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/worker/run-attempts/${attemptId}/heartbeat`, {
    method: "POST",
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
      metadata: {
        daemon: "bucephalus-cloud-worker",
      },
    },
  }) as RunnerInstance;
}

async function heartbeatRunnerInstance(config: WorkerConfig): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/runner-instances/${runnerInstanceId}/heartbeat`, {
    method: "POST",
    body: {
      capabilities: config.capabilities,
      metadata: {
        daemon: "bucephalus-cloud-worker",
      },
    },
  });
}

async function materializePackage(
  config: WorkerConfig,
  claim: RunClaim,
): Promise<MaterializedPackage> {
  const workspaceDir = join(config.dataDir, "worker-runs", claim.run.run_id, claim.attempt.attempt_id);
  const packageArchivePath = join(workspaceDir, "package.tgz");
  const extractedDir = join(workspaceDir, "package");
  const runRootDir = join(workspaceDir, "run-root");
  await rm(workspaceDir, { recursive: true, force: true });
  await mkdir(extractedDir, { recursive: true });
  await mkdir(runRootDir, { recursive: true });
  const packageBytes = await cloudFetchBytes(config, `/v1/packages/${encodeURIComponent(claim.run.package_digest)}/content`);
  await writeFile(packageArchivePath, packageBytes);
  await tar.x({
    file: packageArchivePath,
    cwd: extractedDir,
    strip: 0,
  });

  const manifestJson = JSON.parse(await readFile(join(extractedDir, "manifest.json"), "utf8")) as JsonObject;
  await writeFile(
    join(workspaceDir, "run-env.json"),
    `${JSON.stringify({
      env: claim.run.env,
      secret_refs: claim.run.secret_refs,
      runtime_options: claim.run.runtime_options,
    }, null, 2)}\n`,
  );

  return {
    workspaceDir,
    packageArchivePath,
    extractedDir,
    runRootDir,
    manifestJson,
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
    body: {
      runner_instance_id: requireRunnerInstanceId(config),
      event_type: eventType,
      payload,
    },
  });
}

async function complete(config: WorkerConfig, attemptId: string): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/worker/run-attempts/${attemptId}/complete`, {
    method: "POST",
    body: {
      runner_instance_id: runnerInstanceId,
    },
  });
}

async function fail(config: WorkerConfig, attemptId: string, message: string): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/worker/run-attempts/${attemptId}/fail`, {
    method: "POST",
    body: {
      runner_instance_id: runnerInstanceId,
      message,
    },
  });
}

async function cloudFetch(
  config: WorkerConfig,
  path: string,
  options: { method?: string; body?: unknown } = {},
): Promise<unknown> {
  const init: RequestInit = {
    method: options.method ?? "GET",
    headers: workerAuthHeaders(config),
  };
  if (options.body !== undefined) {
    init.headers = { ...workerAuthHeaders(config), "content-type": "application/json" };
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

async function cloudFetchBytes(config: WorkerConfig, path: string): Promise<Uint8Array> {
  const response = await fetch(`${config.apiUrl}${path}`, {
    headers: workerAuthHeaders(config),
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

function loadWorkerConfig(env: NodeJS.ProcessEnv = process.env): WorkerConfig {
  const appConfig = loadConfig(env);
  const apiUrl = env.BUCEPHALUS_CLOUD_API_URL ?? "http://localhost:8099";
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
    dataDir: appConfig.dataDir,
    coreRunnerCommand: env.BUCEPHALUS_CORE_RUNNER_CMD ?? "bucephalus",
    workerToken: requiredEnv(env.BUCEPHALUS_CLOUD_WORKER_TOKEN, "BUCEPHALUS_CLOUD_WORKER_TOKEN"),
    secretDir: env.BUCEPHALUS_WORKER_SECRET_DIR ?? null,
    capabilities: workerCapabilities(env),
  };
}

function workerAuthHeaders(config: WorkerConfig): Record<string, string> {
  return {
    authorization: `Bearer ${config.workerToken}`,
  };
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
  return {
    executors: csvEnv(env.BUCEPHALUS_WORKER_EXECUTORS, ["runner-docker"]),
    resources: csvEnv(env.BUCEPHALUS_WORKER_RESOURCES, ["core_runner", "docker_daemon", "registry_pull"]),
  };
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

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
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
  };
}

interface RunnerInstance {
  runner_instance_id: string;
}

interface RunRequirements {
  executor: string;
  requires: string[];
  image_refs: string[];
}

interface WorkerCapabilities {
  executors: string[];
  resources: string[];
}

interface MaterializedPackage {
  workspaceDir: string;
  packageArchivePath: string;
  extractedDir: string;
  runRootDir: string;
  manifestJson: JsonObject;
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

main().catch((error) => {
  console.error(errorMessage(error));
  process.exit(1);
});
