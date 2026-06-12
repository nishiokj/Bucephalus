import { HttpError, jsonResponse, queryInteger, readJsonObject, requireBearerToken, requireStaticToken, requireString } from "../http";
import { logWarn, newTraceContext } from "../logging";
import type { JsonObject, JsonValue } from "../primitives";
import { releaseIdentity } from "../release";
import {
  RunnerRepository,
  type RunnerInstanceRecord,
  type RunnerPoolRecord,
} from "../runners/repository";
import type { WorkerCapabilities } from "../packages/repository";
import { optionalJsonObject } from "../packages/repository";

export async function handleRunnerRoute(
  request: Request,
  url: URL,
  runners: RunnerRepository,
  tokens: { workerToken: string; adminToken?: string },
): Promise<Response | null> {
  const workerToken = tokens.workerToken;
  const adminToken = tokens.adminToken ?? workerToken;
  const requireRunnerAdmin = (scope: string) => requireRunnerAdminToken(request, {
    adminToken,
    acceptsLegacyWorkerHeader: adminToken === workerToken,
    scope,
  });
  if (request.method === "POST" && url.pathname === "/v1/runner-pools") {
    requireRunnerAdmin("runner pool management");
    const body = await readJsonObject(request);
    const pool = await runners.createPool({
      name: requireString(body.name, "/name"),
      capabilities: capabilitiesFromBody(body.capabilities),
      metadata: optionalJsonObject(body.metadata as JsonValue | undefined, "/metadata"),
    });
    return jsonResponse(poolToWire(pool), { status: 201 });
  }

  if (request.method === "GET" && url.pathname === "/v1/runner-pools") {
    requireRunnerAdmin("runner pool management");
    const pools = await runners.listPools();
    return jsonResponse({ runner_pools: pools.map(poolToWire) });
  }

  if (request.method === "GET" && url.pathname === "/v1/runner-instances") {
    requireRunnerAdmin("runner instance management");
    const runnerPoolId = url.searchParams.get("runner_pool_id");
    const instances = await runners.listInstances({
      ...(runnerPoolId ? { runnerPoolId } : {}),
      limit: limitFromUrl(url),
    });
    return jsonResponse({ runner_instances: instances.map(instanceToWire) });
  }

  if (request.method === "POST" && url.pathname === "/v1/runner-instances/expire-stale") {
    requireRunnerAdmin("runner instance management");
    const body = await readJsonObject(request).catch(() => ({}));
    const staleAfterSeconds = positiveInt((body as Record<string, unknown>).stale_after_seconds) ?? 90;
    const runnerPoolId = (body as Record<string, unknown>).runner_pool_id;
    const instances = await runners.markStaleInstancesOffline({
      staleAfterSeconds,
      ...(typeof runnerPoolId === "string" && runnerPoolId ? { runnerPoolId } : {}),
    });
    return jsonResponse({ runner_instances: instances.map(instanceToWire) });
  }

  if (request.method === "GET" && url.pathname === "/v1/runner-provision-requests") {
    requireRunnerAdmin("runner provision request management");
    const runnerPoolId = url.searchParams.get("runner_pool_id");
    const requests = await runners.listProvisionRequests({
      ...(runnerPoolId ? { runnerPoolId } : {}),
      limit: limitFromUrl(url),
    });
    return jsonResponse({ provision_requests: requests.map(provisionRequestToWire) });
  }

  if (request.method === "GET" && url.pathname.startsWith("/v1/runner-pools/")) {
    requireRunnerAdmin("runner pool management");
    const poolId = decodeURIComponent(url.pathname.slice("/v1/runner-pools/".length));
    const pool = await runners.getPool(poolId);
    if (!pool) {
      throw new HttpError(404, "runner_pool_not_found", "Runner pool not found");
    }
    return jsonResponse(poolToWire(pool));
  }

  if (request.method === "POST" && url.pathname.startsWith("/v1/runner-pools/") && url.pathname.endsWith("/drain")) {
    requireRunnerAdmin("runner pool management");
    const poolId = decodeURIComponent(url.pathname.slice("/v1/runner-pools/".length, -"/drain".length));
    const pool = await runners.setPoolStatus({ poolId, status: "draining" });
    return jsonResponse(poolToWire(pool));
  }

  if (request.method === "POST" && url.pathname.startsWith("/v1/runner-pools/") && url.pathname.endsWith("/disable")) {
    requireRunnerAdmin("runner pool management");
    const poolId = decodeURIComponent(url.pathname.slice("/v1/runner-pools/".length, -"/disable".length));
    const pool = await runners.setPoolStatus({ poolId, status: "disabled" });
    return jsonResponse(poolToWire(pool));
  }

  if (request.method === "POST" && url.pathname === "/v1/runner-instances/register") {
    requireBearerToken(request, workerToken, "runner instance registration");
    const body = await readJsonObject(request);
    const metadata = optionalJsonObject(body.metadata as JsonValue | undefined, "/metadata");
    const instance = await runners.registerInstance({
      runnerPoolId: requireString(body.runner_pool_id, "/runner_pool_id"),
      instanceName: requireString(body.instance_name, "/instance_name"),
      capabilities: capabilitiesFromBody(body.capabilities),
      metadata,
    });
    warnOnReleaseSkew(instance, metadata);
    return jsonResponse(instanceToWire(instance), { status: 201 });
  }

  if (request.method === "POST" && runnerInstancePath(url.pathname, "/heartbeat")) {
    requireBearerToken(request, workerToken, "runner instance heartbeat");
    const body = await readJsonObject(request);
    const heartbeatInput: {
      runnerInstanceId: string;
      capabilities?: WorkerCapabilities;
      metadata?: JsonObject;
    } = {
      runnerInstanceId: runnerInstanceIdFromPath(url.pathname, "/heartbeat"),
    };
    if (body.capabilities !== undefined) {
      heartbeatInput.capabilities = capabilitiesFromBody(body.capabilities);
    }
    if (body.metadata !== undefined) {
      heartbeatInput.metadata = optionalJsonObject(body.metadata as JsonValue | undefined, "/metadata");
    }
    const instance = await runners.heartbeatInstance(heartbeatInput);
    return jsonResponse(instanceToWire(instance));
  }

  if (request.method === "POST" && runnerInstancePath(url.pathname, "/drain")) {
    requireRunnerAdmin("runner instance management");
    const instance = await runners.setInstanceStatus({
      runnerInstanceId: runnerInstanceIdFromPath(url.pathname, "/drain"),
      status: "draining",
    });
    return jsonResponse(instanceToWire(instance));
  }

  if (request.method === "POST" && runnerInstancePath(url.pathname, "/unhealthy")) {
    requireBearerToken(request, workerToken, "runner instance management");
    const body = await readJsonObject(request);
    const reason = requireString(body.reason, "/reason");
    const instance = await runners.setInstanceStatus({
      runnerInstanceId: runnerInstanceIdFromPath(url.pathname, "/unhealthy"),
      status: "unhealthy",
      metadataPatch: {
        health: {
          status: "unhealthy",
          reason,
          recorded_at: new Date().toISOString(),
          details: optionalJsonObject(body.details as JsonValue | undefined, "/details"),
        },
      },
    });
    return jsonResponse(instanceToWire(instance));
  }

  if (request.method === "POST" && runnerInstancePath(url.pathname, "/offline")) {
    requireBearerToken(request, workerToken, "runner instance management");
    const body = await readJsonObject(request);
    const instance = await runners.setInstanceStatus({
      runnerInstanceId: runnerInstanceIdFromPath(url.pathname, "/offline"),
      status: "offline",
      metadataPatch: {
        last_offline: {
          reason: typeof body.reason === "string" ? body.reason : "worker_shutdown",
          recorded_at: new Date().toISOString(),
        },
      },
    });
    return jsonResponse(instanceToWire(instance));
  }

  return null;
}

function requireRunnerAdminToken(
  request: Request,
  input: { adminToken: string; acceptsLegacyWorkerHeader: boolean; scope: string },
): void {
  requireStaticToken(request, input.adminToken, {
    scope: input.scope,
    credentialName: "runner admin token",
    headerNames: input.acceptsLegacyWorkerHeader
      ? ["x-bucephalus-runner-admin-token", "x-bucephalus-worker-token"]
      : ["x-bucephalus-runner-admin-token"],
  });
}

function poolToWire(pool: RunnerPoolRecord): JsonObject {
  return {
    runner_pool_id: pool.runner_pool_id,
    name: pool.name,
    status: pool.status,
    active_worker_image_id: pool.active_worker_image_id,
    capabilities: pool.capabilities as unknown as JsonObject,
    metadata: pool.metadata,
    created_at: pool.created_at,
    updated_at: pool.updated_at,
  };
}

function instanceToWire(instance: RunnerInstanceRecord): JsonObject {
  return {
    runner_instance_id: instance.runner_instance_id,
    runner_pool_id: instance.runner_pool_id,
    instance_name: instance.instance_name,
    status: instance.status,
    capabilities: instance.capabilities as unknown as JsonObject,
    metadata: instance.metadata,
    last_heartbeat_at: instance.last_heartbeat_at,
    created_at: instance.created_at,
    updated_at: instance.updated_at,
  };
}

function provisionRequestToWire(request: {
  provision_request_id: string;
  runner_pool_id: string;
  run_id: string | null;
  status: string;
  provider: string;
  provider_instance_id: string | null;
  instance_name: string | null;
  runner_instance_id: string | null;
  requirements: JsonObject;
  metadata: JsonObject;
  error_message: string | null;
  created_at: string;
  updated_at: string;
}): JsonObject {
  return {
    provision_request_id: request.provision_request_id,
    runner_pool_id: request.runner_pool_id,
    run_id: request.run_id,
    status: request.status,
    provider: request.provider,
    provider_instance_id: request.provider_instance_id,
    instance_name: request.instance_name,
    runner_instance_id: request.runner_instance_id,
    requirements: request.requirements,
    metadata: request.metadata,
    error_message: request.error_message,
    created_at: request.created_at,
    updated_at: request.updated_at,
  };
}

function limitFromUrl(url: URL): number {
  return queryInteger(url, "limit", { defaultValue: 100, min: 1, max: 200 });
}

function warnOnReleaseSkew(instance: RunnerInstanceRecord, metadata: JsonObject): void {
  const api = releaseIdentity();
  const workerRelease = isRecord(metadata.release) ? metadata.release : null;
  const workerVersion = typeof workerRelease?.version === "string" ? workerRelease.version : null;
  if (!api.version || !workerVersion || api.version === workerVersion) {
    return;
  }
  logWarn("runner.release_skew", newTraceContext({ component: "api" }), {
    runner_instance_id: instance.runner_instance_id,
    runner_pool_id: instance.runner_pool_id,
    api_release: api,
    worker_release: workerRelease,
    detail: "worker and API are running different releases; package contract validation may disagree between run creation and execution",
  });
}

function capabilitiesFromBody(value: unknown): WorkerCapabilities {
  if (!isRecord(value)) {
    return { executors: [], resources: [] };
  }
  return {
    executors: stringArray(value.executors),
    resources: stringArray(value.resources),
    ...(typeof value.arch === "string" ? { arch: value.arch } : {}),
    ...(positiveInt(value.cpu_count) ? { cpu_count: positiveInt(value.cpu_count) } : {}),
    ...(positiveInt(value.memory_mb) ? { memory_mb: positiveInt(value.memory_mb) } : {}),
    ...(positiveInt(value.disk_mb) ? { disk_mb: positiveInt(value.disk_mb) } : {}),
    ...(Array.isArray(value.isolation) ? { isolation: stringArray(value.isolation) } : {}),
  };
}

function stringArray(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.filter((item): item is string => typeof item === "string" && item.trim().length > 0);
}

function positiveInt(value: unknown): number | null {
  if (typeof value === "number" && Number.isInteger(value) && value > 0) {
    return value;
  }
  if (typeof value === "string" && /^\d+$/.test(value)) {
    const parsed = Number.parseInt(value, 10);
    return parsed > 0 ? parsed : null;
  }
  return null;
}

function runnerInstancePath(pathname: string, suffix: string): boolean {
  return pathname.startsWith("/v1/runner-instances/") && pathname.endsWith(suffix);
}

function runnerInstanceIdFromPath(pathname: string, suffix: string): string {
  return decodeURIComponent(pathname.slice("/v1/runner-instances/".length, -suffix.length));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
