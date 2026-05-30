import { HttpError, jsonResponse, readJsonObject, requireBearerToken, requireString } from "../http";
import type { JsonObject, JsonValue } from "../primitives";
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
  workerToken: string,
): Promise<Response | null> {
  if (request.method === "POST" && url.pathname === "/v1/runner-pools") {
    requireBearerToken(request, workerToken, "runner pool management");
    const body = await readJsonObject(request);
    const pool = await runners.createPool({
      name: requireString(body.name, "/name"),
      capabilities: capabilitiesFromBody(body.capabilities),
      metadata: optionalJsonObject(body.metadata as JsonValue | undefined, "/metadata"),
    });
    return jsonResponse(poolToWire(pool), { status: 201 });
  }

  if (request.method === "GET" && url.pathname === "/v1/runner-pools") {
    requireBearerToken(request, workerToken, "runner pool management");
    const pools = await runners.listPools();
    return jsonResponse({ runner_pools: pools.map(poolToWire) });
  }

  if (request.method === "GET" && url.pathname.startsWith("/v1/runner-pools/")) {
    requireBearerToken(request, workerToken, "runner pool management");
    const poolId = decodeURIComponent(url.pathname.slice("/v1/runner-pools/".length));
    const pool = await runners.getPool(poolId);
    if (!pool) {
      throw new HttpError(404, "runner_pool_not_found", "Runner pool not found");
    }
    return jsonResponse(poolToWire(pool));
  }

  if (request.method === "POST" && url.pathname.startsWith("/v1/runner-pools/") && url.pathname.endsWith("/drain")) {
    requireBearerToken(request, workerToken, "runner pool management");
    const poolId = decodeURIComponent(url.pathname.slice("/v1/runner-pools/".length, -"/drain".length));
    const pool = await runners.setPoolStatus({ poolId, status: "draining" });
    return jsonResponse(poolToWire(pool));
  }

  if (request.method === "POST" && url.pathname === "/v1/runner-instances/register") {
    requireBearerToken(request, workerToken, "runner instance registration");
    const body = await readJsonObject(request);
    const instance = await runners.registerInstance({
      runnerPoolId: requireString(body.runner_pool_id, "/runner_pool_id"),
      instanceName: requireString(body.instance_name, "/instance_name"),
      capabilities: capabilitiesFromBody(body.capabilities),
      metadata: optionalJsonObject(body.metadata as JsonValue | undefined, "/metadata"),
    });
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
    requireBearerToken(request, workerToken, "runner instance management");
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

function poolToWire(pool: RunnerPoolRecord): JsonObject {
  return {
    runner_pool_id: pool.runner_pool_id,
    name: pool.name,
    status: pool.status,
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
