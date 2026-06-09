#!/usr/bin/env bun
import { spawn } from "node:child_process";
import { loadConfig } from "./config";
import { createSql } from "./db/client";
import {
  RunnerRepository,
  type QueuedRunDemandRecord,
  type ReapableRunnerProvisionRequestRecord,
  type RunnerInstanceRecord,
  type RunnerPoolRecord,
  type RunnerProvisionRequestRecord,
} from "./runners/repository";
import type { JsonObject } from "./primitives";
import type { WorkerCapabilities } from "./packages/repository";
import {
  childTraceContext,
  initTelemetry,
  logError,
  logInfo,
  newTraceContext,
  type TraceContext,
} from "./logging";

interface PoolControllerConfig {
  apiUrl: string;
  workerToken: string;
  runnerPoolId: string;
  provider: "exec";
  provisionCommand: string[];
  reapCommand: string[];
  configuredPoolCapabilities: WorkerCapabilities | null;
  pollMs: number;
  staleInstanceSeconds: number;
  provisioningTimeoutSeconds: number;
  providerCommandTimeoutMs: number;
  demandLimit: number;
  reapIdleCompletedRunners: boolean;
  idleReapDelaySeconds: number;
  healthHost: string;
  healthPort: number;
}

interface ProvisionOutput {
  provider_instance_id: string;
  instance_name?: string;
  metadata?: JsonObject;
}

interface ReapOutput {
  metadata?: JsonObject;
}

class PoolControllerError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "PoolControllerError";
  }
}

interface PoolControllerHealthState {
  startedAt: string;
  lastReconcileStartedAt: string | null;
  lastReconcileCompletedAt: string | null;
  lastReconcileError: string | null;
  reconcileCount: number;
}

let shuttingDown = false;

async function main(): Promise<void> {
  await initTelemetry();
  const appConfig = loadConfig();
  const config = loadPoolControllerConfig();
  const serviceContext = newTraceContext({
    component: "pool-controller",
    requestId: `pool-${config.runnerPoolId}`,
  });
  logInfo("pool_controller.starting", serviceContext, {
    runner_pool_id: config.runnerPoolId,
    api_url: config.apiUrl,
    poll_ms: config.pollMs,
  });
  const sql = createSql(appConfig.databaseUrl);
  const runners = new RunnerRepository(sql);
  const healthState: PoolControllerHealthState = {
    startedAt: new Date().toISOString(),
    lastReconcileStartedAt: null,
    lastReconcileCompletedAt: null,
    lastReconcileError: null,
    reconcileCount: 0,
  };
  const healthServer = startPoolControllerHealthServer(config, healthState);

  process.on("SIGINT", () => {
    shuttingDown = true;
  });
  process.on("SIGTERM", () => {
    shuttingDown = true;
  });

  try {
    while (!shuttingDown) {
      healthState.lastReconcileStartedAt = new Date().toISOString();
      const reconcileContext = childTraceContext(serviceContext, {
        component: "pool-controller-reconcile",
      });
      try {
        await reconcileOnce(config, runners, reconcileContext);
        healthState.lastReconcileCompletedAt = new Date().toISOString();
        healthState.lastReconcileError = null;
        healthState.reconcileCount += 1;
      } catch (error) {
        healthState.lastReconcileError = errorMessage(error);
        logError("pool_controller.reconcile_failed", reconcileContext, { error: errorMessage(error) });
        throw error;
      }
      await Bun.sleep(config.pollMs);
    }
  } finally {
    healthServer.stop(true);
    await sql.end({ timeout: 1 });
  }
}

export async function reconcileOnce(
  config: PoolControllerConfig,
  runners: RunnerRepository,
  context: TraceContext = newTraceContext({ component: "pool-controller-reconcile" }),
): Promise<void> {
  let pool = await runners.getPool(config.runnerPoolId);
  if (!pool) {
    throw new PoolControllerError(`runner pool not found: ${config.runnerPoolId}`);
  }
  pool = await reconcilePoolCapabilities(config, runners, pool, context);
  if (pool.status !== "active") {
    return;
  }

  const stale = await runners.markStaleInstancesOffline({
    runnerPoolId: config.runnerPoolId,
    staleAfterSeconds: config.staleInstanceSeconds,
  });
  for (const instance of stale) {
    logInfo("pool_controller.instance_marked_offline", context, {
      runner_instance_id: instance.runner_instance_id,
    });
  }

  const timedOut = await runners.failStaleUnacceptedProvisionRequests({
    runnerPoolId: config.runnerPoolId,
    provisioningTimeoutSeconds: config.provisioningTimeoutSeconds,
  });
  for (const request of timedOut) {
    logInfo("pool_controller.provision_request_timed_out", context, {
      provision_request_id: request.provision_request_id,
    });
  }

  const reapable = await runners.listReapableProvisionRequests({
    runnerPoolId: config.runnerPoolId,
    provisioningTimeoutSeconds: config.provisioningTimeoutSeconds,
    limit: config.demandLimit,
  });
  for (const request of reapable) {
    await reapRunner(config, runners, request, reapContext(context, request));
  }

  if (config.reapIdleCompletedRunners) {
    const idleCompleted = await runners.listIdleCompletedRunProvisionRequests({
      runnerPoolId: config.runnerPoolId,
      idleReapDelaySeconds: config.idleReapDelaySeconds,
      limit: config.demandLimit,
    });
    for (const request of idleCompleted) {
      await reapRunner(config, runners, request, reapContext(context, request));
    }
  }

  const [demand, instances, openProvisionRequests] = await Promise.all([
    runners.listQueuedDemand({ limit: config.demandLimit }),
    runners.listClaimableInstances({
      runnerPoolId: config.runnerPoolId,
      staleAfterSeconds: config.staleInstanceSeconds,
    }),
    runners.listOpenProvisionRequests({ runnerPoolId: config.runnerPoolId }),
  ]);

  for (const run of demand) {
    if (!matchesPool(pool, run)) {
      continue;
    }
    if (hasMatchingInstance(instances, run)) {
      continue;
    }
    if (openProvisionRequests.some((request) => request.run_id === run.run_id)) {
      continue;
    }

    const request = await runners.createProvisionRequest({
      runnerPoolId: pool.runner_pool_id,
      runId: run.run_id,
      provider: config.provider,
      requirements: run.run_requirements as unknown as JsonObject,
      metadata: {
        requested_by: "bucephalus-pool-controller",
        requested_at: new Date().toISOString(),
      },
    });
    if (!request) {
      continue;
    }
    const runContext = childTraceContext(context, {
      component: "pool-controller-provision",
      runId: run.run_id,
    });
    await provisionRunner(config, runners, pool, run, request, runContext);
  }
}

function reapContext(
  parent: TraceContext,
  request: ReapableRunnerProvisionRequestRecord,
): TraceContext {
  return childTraceContext(parent, {
    component: "pool-controller-reap",
    runId: request.run_id ?? undefined,
  });
}

async function reconcilePoolCapabilities(
  config: PoolControllerConfig,
  runners: RunnerRepository,
  pool: RunnerPoolRecord,
  context: TraceContext,
): Promise<RunnerPoolRecord> {
  if (!config.configuredPoolCapabilities) {
    return pool;
  }
  if (capabilitiesEqual(pool.capabilities, config.configuredPoolCapabilities)) {
    return pool;
  }
  const updated = await runners.setPoolCapabilities({
    poolId: pool.runner_pool_id,
    capabilities: config.configuredPoolCapabilities,
  });
  logInfo("pool_controller.capabilities_reconciled", context, {
    runner_pool_id: pool.runner_pool_id,
  });
  return updated;
}

async function reapRunner(
  config: PoolControllerConfig,
  runners: RunnerRepository,
  request: ReapableRunnerProvisionRequestRecord,
  context: TraceContext,
): Promise<void> {
  try {
    const output = await runReapCommand(config, request);
    await runners.markProvisionRequestReaped({
      provisionRequestId: request.provision_request_id,
      metadata: {
        reaped_at: new Date().toISOString(),
        provider_output: output.metadata ?? {},
      },
    });
    logInfo("pool_controller.provision_reaped", context, {
      provision_request_id: request.provision_request_id,
      provider_instance_id: request.provider_instance_id,
    });
  } catch (error) {
    logError("pool_controller.reap_failed", context, {
      provision_request_id: request.provision_request_id,
      error: errorMessage(error),
    });
  }
}

async function provisionRunner(
  config: PoolControllerConfig,
  runners: RunnerRepository,
  pool: RunnerPoolRecord,
  run: QueuedRunDemandRecord,
  request: RunnerProvisionRequestRecord,
  context: TraceContext,
): Promise<void> {
  try {
    await runners.markProvisioning({
      provisionRequestId: request.provision_request_id,
      metadata: {
        provider_started_at: new Date().toISOString(),
      },
    });
    const output = await runProvisionCommand(config, pool, run, request);
    await runners.markProvisioning({
      provisionRequestId: request.provision_request_id,
      providerInstanceId: output.provider_instance_id,
      instanceName: output.instance_name ?? null,
      metadata: {
        provider_output: output.metadata ?? {},
        provisioned_at: new Date().toISOString(),
      },
    });
    logInfo("pool_controller.provision_requested", context, {
      run_id: run.run_id,
      provision_request_id: request.provision_request_id,
      provider_instance_id: output.provider_instance_id,
    });
  } catch (error) {
    await runners.failProvisionRequest({
      provisionRequestId: request.provision_request_id,
      message: errorMessage(error),
      metadata: {
        failed_at: new Date().toISOString(),
      },
    });
    logError("pool_controller.provision_failed", context, {
      run_id: run.run_id,
      provision_request_id: request.provision_request_id,
      error: errorMessage(error),
    });
  }
}

async function runReapCommand(
  config: PoolControllerConfig,
  request: ReapableRunnerProvisionRequestRecord,
): Promise<ReapOutput> {
  const [executable, ...args] = config.reapCommand;
  if (!executable) {
    throw new PoolControllerError("reap command is empty");
  }
  const input = {
    api_url: config.apiUrl,
    runner_pool_id: request.runner_pool_id,
    provision_request_id: request.provision_request_id,
    run_id: request.run_id,
    provider: request.provider,
    provider_instance_id: request.provider_instance_id,
    instance_name: request.instance_name,
    runner_instance_id: request.runner_instance_id,
    runner_instance_status: request.runner_instance_status,
    runner_instance_metadata: request.runner_instance_metadata ?? {},
    run_status: request.run_status ?? null,
    requirements: request.requirements,
    metadata: request.metadata,
  };
  const result = await runJsonCommand(
    executable,
    args,
    input,
    {
      BUCEPHALUS_CLOUD_API_URL: config.apiUrl,
      BUCEPHALUS_CLOUD_WORKER_TOKEN: config.workerToken,
      BUCEPHALUS_RUNNER_POOL_ID: request.runner_pool_id,
      BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID: request.provision_request_id,
    },
    config.providerCommandTimeoutMs,
  );
  if (!isRecord(result)) {
    throw new PoolControllerError("reap command must return a JSON object");
  }
  const output: ReapOutput = {};
  if (isRecord(result.metadata)) {
    output.metadata = result.metadata;
  }
  return output;
}

async function runProvisionCommand(
  config: PoolControllerConfig,
  pool: RunnerPoolRecord,
  run: QueuedRunDemandRecord,
  request: RunnerProvisionRequestRecord,
): Promise<ProvisionOutput> {
  const [executable, ...args] = config.provisionCommand;
  if (!executable) {
    throw new PoolControllerError("provision command is empty");
  }
  const input = {
    api_url: config.apiUrl,
    runner_pool_id: pool.runner_pool_id,
    provision_request_id: request.provision_request_id,
    run_id: run.run_id,
    run_requirements: run.run_requirements,
    worker_env: {
      BUCEPHALUS_CLOUD_API_URL: config.apiUrl,
      BUCEPHALUS_RUNNER_POOL_ID: pool.runner_pool_id,
      BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID: request.provision_request_id,
    },
  };
  const result = await runJsonCommand(
    executable,
    args,
    input,
    {
      BUCEPHALUS_CLOUD_API_URL: config.apiUrl,
      BUCEPHALUS_CLOUD_WORKER_TOKEN: config.workerToken,
      BUCEPHALUS_RUNNER_POOL_ID: pool.runner_pool_id,
      BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID: request.provision_request_id,
    },
    config.providerCommandTimeoutMs,
  );
  if (!isRecord(result)) {
    throw new PoolControllerError("provision command must return a JSON object");
  }
  const providerInstanceId = result.provider_instance_id;
  if (typeof providerInstanceId !== "string" || providerInstanceId.trim().length === 0) {
    throw new PoolControllerError("provision command output requires provider_instance_id");
  }
  const output: ProvisionOutput = {
    provider_instance_id: providerInstanceId.trim(),
  };
  if (typeof result.instance_name === "string" && result.instance_name.trim().length > 0) {
    output.instance_name = result.instance_name.trim();
  }
  if (isRecord(result.metadata)) {
    output.metadata = result.metadata;
  }
  return output;
}

async function runJsonCommand(
  executable: string,
  args: string[],
  input: JsonObject,
  extraEnv: Record<string, string>,
  timeoutMs: number,
): Promise<unknown> {
  const result = await new Promise<{ exitCode: number; stdout: string; stderr: string }>((resolve, reject) => {
    const child = spawn(executable, args, {
      env: {
        ...process.env,
        ...extraEnv,
      },
      detached: true,
      stdio: ["pipe", "pipe", "pipe"],
    });
    let settled = false;
    const timeout = globalThis.setTimeout(() => {
      if (settled) {
        return;
      }
      settled = true;
      signalProcessGroup(child, "SIGKILL");
      reject(new PoolControllerError(`${executable} timed out after ${timeoutMs}ms`));
    }, timeoutMs);
    timeout.unref?.();
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    child.stdout.on("data", (chunk: Buffer) => stdout.push(chunk));
    child.stderr.on("data", (chunk: Buffer) => stderr.push(chunk));
    child.on("error", (error) => {
      if (settled) {
        return;
      }
      settled = true;
      globalThis.clearTimeout(timeout);
      reject(error);
    });
    child.on("close", (code) => {
      if (settled) {
        return;
      }
      settled = true;
      globalThis.clearTimeout(timeout);
      resolve({
        exitCode: code ?? 1,
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
      });
    });
    child.stdin.end(`${JSON.stringify(input)}\n`);
  });
  if (result.exitCode !== 0) {
    throw new PoolControllerError(`${executable} exited ${result.exitCode}: ${tail(result.stderr || result.stdout, 1000)}`);
  }
  try {
    return JSON.parse(result.stdout);
  } catch (error) {
    throw new PoolControllerError(`provision command returned invalid JSON: ${errorMessage(error)}`);
  }
}

function signalProcessGroup(child: ReturnType<typeof spawn>, signal: NodeJS.Signals): void {
  if (!child.pid) {
    return;
  }
  try {
    process.kill(-child.pid, signal);
  } catch {
    child.kill(signal);
  }
}

export function matchesCapabilities(
  capabilities: {
    executors: string[];
    resources: string[];
    arch?: string | null;
    cpu_count?: number | null;
    memory_mb?: number | null;
    disk_mb?: number | null;
    isolation?: string[];
  },
  requirements: {
    executor: string;
    requires: string[];
    arch?: string;
    cpu_count?: number;
    memory_mb?: number;
    disk_mb?: number;
    isolation?: string;
  },
): boolean {
  return capabilities.executors.includes(requirements.executor)
    && requirements.requires.every((resource) => capabilities.resources.includes(resource))
    && (!requirements.arch || !capabilities.arch || capabilities.arch === requirements.arch)
    && (!requirements.cpu_count || !capabilities.cpu_count || capabilities.cpu_count >= requirements.cpu_count)
    && (!requirements.memory_mb || !capabilities.memory_mb || capabilities.memory_mb >= requirements.memory_mb)
    && (!requirements.disk_mb || !capabilities.disk_mb || capabilities.disk_mb >= requirements.disk_mb)
    && (!requirements.isolation || !capabilities.isolation || capabilities.isolation.length === 0 || capabilities.isolation.includes(requirements.isolation));
}

function matchesPool(pool: RunnerPoolRecord, run: QueuedRunDemandRecord): boolean {
  return matchesCapabilities(pool.capabilities, run.run_requirements);
}

function hasMatchingInstance(instances: RunnerInstanceRecord[], run: QueuedRunDemandRecord): boolean {
  return instances.some((instance) => matchesCapabilities(instance.capabilities, run.run_requirements));
}

function loadPoolControllerConfig(env: NodeJS.ProcessEnv = process.env): PoolControllerConfig {
  const appConfig = loadConfig(env);
  const provider = (env.BUCEPHALUS_POOL_CONTROLLER_PROVIDER ?? "exec").trim();
  if (provider !== "exec") {
    throw new PoolControllerError(`unsupported pool controller provider: ${provider}`);
  }
  return {
    apiUrl: requiredEnv(env.BUCEPHALUS_CLOUD_API_URL, "BUCEPHALUS_CLOUD_API_URL").replace(/\/+$/, ""),
    workerToken: requiredEnv(env.BUCEPHALUS_CLOUD_WORKER_TOKEN, "BUCEPHALUS_CLOUD_WORKER_TOKEN"),
    runnerPoolId: requiredEnv(env.BUCEPHALUS_POOL_CONTROLLER_POOL_ID, "BUCEPHALUS_POOL_CONTROLLER_POOL_ID"),
    provider,
    provisionCommand: parseCommandJson(requiredEnv(env.BUCEPHALUS_POOL_CONTROLLER_PROVISION_CMD_JSON, "BUCEPHALUS_POOL_CONTROLLER_PROVISION_CMD_JSON")),
    reapCommand: parseCommandJson(requiredEnv(env.BUCEPHALUS_POOL_CONTROLLER_REAP_CMD_JSON, "BUCEPHALUS_POOL_CONTROLLER_REAP_CMD_JSON")),
    configuredPoolCapabilities: parseCapabilitiesJson(env.BUCEPHALUS_POOL_CONTROLLER_CAPABILITIES_JSON),
    pollMs: numberEnv(env.BUCEPHALUS_POOL_CONTROLLER_POLL_MS, 2000),
    staleInstanceSeconds: numberEnv(env.BUCEPHALUS_POOL_CONTROLLER_STALE_INSTANCE_SECONDS, 90),
    provisioningTimeoutSeconds: numberEnv(env.BUCEPHALUS_POOL_CONTROLLER_PROVISIONING_TIMEOUT_SECONDS, 600),
    providerCommandTimeoutMs: Math.max(1, numberEnv(
      env.BUCEPHALUS_POOL_CONTROLLER_PROVIDER_CMD_TIMEOUT_MS,
      Math.max(1, numberEnv(env.BUCEPHALUS_POOL_CONTROLLER_PROVISIONING_TIMEOUT_SECONDS, 600)) * 1000,
    )),
    demandLimit: numberEnv(env.BUCEPHALUS_POOL_CONTROLLER_DEMAND_LIMIT, 50),
    reapIdleCompletedRunners: booleanEnv(env.BUCEPHALUS_POOL_CONTROLLER_REAP_IDLE_COMPLETED_RUNNERS, true),
    idleReapDelaySeconds: Math.max(0, numberEnv(env.BUCEPHALUS_POOL_CONTROLLER_IDLE_REAP_DELAY_SECONDS, 0)),
    healthHost: env.BUCEPHALUS_POOL_CONTROLLER_HEALTH_HOST?.trim() || appConfig.host,
    healthPort: numberEnv(env.PORT ?? env.BUCEPHALUS_POOL_CONTROLLER_HEALTH_PORT, appConfig.port),
  };
}

function parseCapabilitiesJson(raw: string | undefined): WorkerCapabilities | null {
  if (!raw || raw.trim().length === 0) {
    return null;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new PoolControllerError(`invalid BUCEPHALUS_POOL_CONTROLLER_CAPABILITIES_JSON: ${errorMessage(error)}`);
  }
  if (!isRecord(parsed)) {
    throw new PoolControllerError("BUCEPHALUS_POOL_CONTROLLER_CAPABILITIES_JSON must be a JSON object");
  }
  return {
    executors: stringArray(parsed.executors, "executors"),
    resources: stringArray(parsed.resources, "resources"),
    ...(typeof parsed.arch === "string" && parsed.arch.trim().length > 0 ? { arch: parsed.arch.trim() } : {}),
    ...(positiveInteger(parsed.cpu_count) ? { cpu_count: positiveInteger(parsed.cpu_count) } : {}),
    ...(positiveInteger(parsed.memory_mb) ? { memory_mb: positiveInteger(parsed.memory_mb) } : {}),
    ...(positiveInteger(parsed.disk_mb) ? { disk_mb: positiveInteger(parsed.disk_mb) } : {}),
    ...(Array.isArray(parsed.isolation) ? { isolation: stringArray(parsed.isolation, "isolation") } : {}),
  };
}

function stringArray(value: unknown, name: string): string[] {
  if (!Array.isArray(value) || value.length === 0) {
    throw new PoolControllerError(`BUCEPHALUS_POOL_CONTROLLER_CAPABILITIES_JSON.${name} must be a non-empty string array`);
  }
  const result = value.map((item) => typeof item === "string" ? item.trim() : "").filter(Boolean);
  if (result.length !== value.length) {
    throw new PoolControllerError(`BUCEPHALUS_POOL_CONTROLLER_CAPABILITIES_JSON.${name} must be a non-empty string array`);
  }
  return [...new Set(result)];
}

function positiveInteger(value: unknown): number | null {
  return typeof value === "number" && Number.isInteger(value) && value > 0 ? value : null;
}

function capabilitiesEqual(left: WorkerCapabilities, right: WorkerCapabilities): boolean {
  return JSON.stringify(normalizedCapabilities(left)) === JSON.stringify(normalizedCapabilities(right));
}

function normalizedCapabilities(capabilities: WorkerCapabilities): WorkerCapabilities {
  return {
    executors: [...new Set(capabilities.executors)].sort(),
    resources: [...new Set(capabilities.resources)].sort(),
    ...(capabilities.arch ? { arch: capabilities.arch } : {}),
    ...(capabilities.cpu_count ? { cpu_count: capabilities.cpu_count } : {}),
    ...(capabilities.memory_mb ? { memory_mb: capabilities.memory_mb } : {}),
    ...(capabilities.disk_mb ? { disk_mb: capabilities.disk_mb } : {}),
    ...(capabilities.isolation ? { isolation: [...new Set(capabilities.isolation)].sort() } : {}),
  };
}

function parseCommandJson(raw: string): string[] {
  const parsed = JSON.parse(raw);
  if (!Array.isArray(parsed) || parsed.some((item) => typeof item !== "string" || item.trim().length === 0)) {
    throw new PoolControllerError("BUCEPHALUS_POOL_CONTROLLER_PROVISION_CMD_JSON must be a JSON string array");
  }
  return parsed;
}

function requiredEnv(value: string | undefined, name: string): string {
  if (!value || value.trim().length === 0) {
    throw new PoolControllerError(`${name} is required`);
  }
  return value.trim();
}

function numberEnv(value: string | undefined, fallback: number): number {
  if (!value) {
    return fallback;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function booleanEnv(value: string | undefined, fallback: boolean): boolean {
  if (!value) {
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

function tail(value: string, maxBytes: number): string {
  const buffer = Buffer.from(value, "utf8");
  if (buffer.byteLength <= maxBytes) {
    return value;
  }
  return buffer.subarray(buffer.byteLength - maxBytes).toString("utf8");
}

function isRecord(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function startPoolControllerHealthServer(
  config: Pick<PoolControllerConfig, "healthHost" | "healthPort">,
  state: PoolControllerHealthState,
): ReturnType<typeof Bun.serve> {
  return Bun.serve({
    hostname: config.healthHost,
    port: config.healthPort,
    fetch(request) {
      const url = new URL(request.url);
      if (url.pathname === "/healthz" || url.pathname === "/readyz") {
        const ready = !shuttingDown;
        return Response.json(
          {
            ok: ready,
            service: "bucephalus-pool-controller",
            started_at: state.startedAt,
            last_reconcile_started_at: state.lastReconcileStartedAt,
            last_reconcile_completed_at: state.lastReconcileCompletedAt,
            last_reconcile_error: state.lastReconcileError,
            reconcile_count: state.reconcileCount,
            shutting_down: shuttingDown,
          },
          { status: ready ? 200 : 503 },
        );
      }
      return new Response("not found\n", { status: 404 });
    },
  });
}

if (import.meta.main) {
  main().catch((error) => {
    logError("pool_controller.fatal", newTraceContext({ component: "pool-controller" }), {
      error: errorMessage(error),
    });
    process.exit(1);
  });
}
