#!/usr/bin/env bun
import { mkdir, readdir, readFile, rm, statfs, writeFile } from "node:fs/promises";
import type { Dirent } from "node:fs";
import { randomUUID } from "node:crypto";
import { join } from "node:path";
import os from "node:os";
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
  minFreeBytes: number;
  retainAttemptWorkspaces: boolean;
  provisionRequestId: string | null;
  providerInstanceId: string | null;
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
      await heartbeat(config, attemptId);
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
      await executeCoreRun(config, claim, materialized);
    } catch (error) {
      coreError = error;
    }

    try {
      await cleanupClaimWorkspace(config, claim, materialized ?? {
        workspaceDir,
        packageArchivePath: join(workspaceDir, "package.tgz"),
        extractedDir: join(workspaceDir, "package"),
        runRootDir: join(workspaceDir, "run-root"),
        manifestJson: {},
      });
    } catch (error) {
      cleanupError = error;
    }

    if (cleanupError) {
      const message = `runner cleanup failed after run ${runId} attempt ${attemptId}: ${errorMessage(cleanupError)}`;
      await fail(config, attemptId, message).catch((failError) => {
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
      await fail(config, attemptId, errorMessage(coreError)).catch((failError) => {
        console.error(`worker ${config.workerId} failed to mark run failed: ${errorMessage(failError)}`);
      });
    } else {
      await complete(config, attemptId);
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

async function materializePackage(
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
  const runsDir = join(runRootDir, ".lab", "runs");
  let entries: Dirent[];
  try {
    entries = await readdir(runsDir, { withFileTypes: true });
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
  const listArgs = kind === "container" ? ["ps", "-aq"] : [kind, "ls", "-q"];
  for (const filter of filters) {
    listArgs.push("--filter", filter);
  }
  const listed = await runCommand("docker", listArgs);
  const ids = listed.stdout.split(/\s+/).map((item) => item.trim()).filter(Boolean);
  if (ids.length === 0) {
    return 0;
  }
  const removeArgs = kind === "container" ? ["rm", "-f", ...ids] : [kind, "rm", ...ids];
  await runCommand("docker", removeArgs);
  return ids.length;
}

async function runCommand(
  executable: string,
  args: string[],
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  const result = await new Promise<{ exitCode: number; stdout: string; stderr: string }>((resolve, reject) => {
    const child = spawn(executable, args, {
      stdio: ["ignore", "pipe", "pipe"],
    });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    child.stdout.on("data", (chunk: Buffer) => stdout.push(chunk));
    child.stderr.on("data", (chunk: Buffer) => stderr.push(chunk));
    child.on("error", reject);
    child.on("close", (code) => {
      resolve({
        exitCode: code ?? 1,
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
      });
    });
  });
  if (result.exitCode !== 0) {
    throw new WorkerError(`${executable} ${args.join(" ")} exited ${result.exitCode}: ${tail(result.stderr || result.stdout, 1000)}`);
  }
  return result;
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
    await runCommand("docker", ["version"]);
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
    minFreeBytes: numberEnv(
      env.BUCEPHALUS_WORKER_MIN_FREE_BYTES ?? env.BUCEPHALUS_MIN_FREE_BYTES,
      20 * 1024 * 1024 * 1024,
    ),
    retainAttemptWorkspaces: booleanEnv(env.BUCEPHALUS_WORKER_RETAIN_ATTEMPT_WORKSPACES, false),
    provisionRequestId: env.BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID?.trim() || null,
    providerInstanceId: env.BUCEPHALUS_RUNNER_PROVIDER_INSTANCE_ID?.trim() || null,
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
  };
}

interface RunnerInstance {
  runner_instance_id: string;
}

interface RunRequirements {
  executor: string;
  requires: string[];
  image_refs: string[];
  arch?: string;
  cpu_count?: number;
  memory_mb?: number;
  disk_mb?: number;
  isolation?: string;
  timeout_ms?: number | null;
  max_parallel_trials?: number;
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
