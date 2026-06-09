import { decodePathParam, HttpError, jsonResponse, queryIntegerParam, readJsonObject, readOptionalJsonObject, requireBearerToken, requireString } from "../http";
import type { JsonObject, JsonValue } from "../primitives";
import {
  RunnerRepository,
  type RunnerInstanceRecord,
  type RunnerPoolRecord,
} from "../runners/repository";
import type { WorkerCapabilities } from "../packages/repository";
import { optionalJsonObject } from "../packages/repository";
import { publicBoundaryJsonObject, publicBoundaryText } from "../publicBoundary";

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const MAX_RUNNER_CAPABILITY_INT = 2_147_483_647;

export async function handleRunnerRoute(
  request: Request,
  url: URL,
  runners: RunnerRepository,
  tokens: { workerToken: string; adminToken?: string },
): Promise<Response | null> {
  const workerToken = tokens.workerToken;
  const adminToken = tokens.adminToken ?? workerToken;
  if (request.method === "POST" && url.pathname === "/v1/runner-pools") {
    requireBearerToken(request, adminToken, "runner pool management");
    const body = await readJsonObject(request);
    const pool = await runners.createPool({
      name: requireString(body.name, "/name"),
      capabilities: capabilitiesFromBody(body.capabilities),
      metadata: optionalJsonObject(body.metadata as JsonValue | undefined, "/metadata"),
    });
    return jsonResponse(poolToWire(pool), { status: 201 });
  }

  if (request.method === "GET" && url.pathname === "/v1/runner-pools") {
    requireBearerToken(request, adminToken, "runner pool management");
    const pools = await runners.listPools();
    return jsonResponse({ runner_pools: pools.map(poolToWire) });
  }

  if (request.method === "GET" && url.pathname === "/v1/runner-instances") {
    requireBearerToken(request, adminToken, "runner instance management");
    const runnerPoolId = optionalUuidString(url.searchParams.get("runner_pool_id"), "/runner_pool_id");
    const instances = await runners.listInstances({
      ...(runnerPoolId !== null ? { runnerPoolId } : {}),
      limit: limitFromUrl(url),
    });
    return jsonResponse({ runner_instances: instances.map(instanceToWire) });
  }

  if (request.method === "POST" && url.pathname === "/v1/runner-instances/expire-stale") {
    requireBearerToken(request, adminToken, "runner instance management");
    const body = await readOptionalJsonObject(request);
    const staleAfterSeconds = optionalPositiveInt(
      body.stale_after_seconds,
      "/stale_after_seconds",
      90,
    );
    const runnerPoolId = optionalUuidString(body.runner_pool_id, "/runner_pool_id");
    const instances = await runners.markStaleInstancesOffline({
      staleAfterSeconds,
      ...(runnerPoolId !== null ? { runnerPoolId } : {}),
    });
    return jsonResponse({ runner_instances: instances.map(instanceToWire) });
  }

  if (request.method === "GET" && url.pathname === "/v1/runner-provision-requests") {
    requireBearerToken(request, adminToken, "runner provision request management");
    const runnerPoolId = optionalUuidString(url.searchParams.get("runner_pool_id"), "/runner_pool_id");
    const requests = await runners.listProvisionRequests({
      ...(runnerPoolId !== null ? { runnerPoolId } : {}),
      limit: limitFromUrl(url),
    });
    return jsonResponse({ provision_requests: requests.map(provisionRequestToWire) });
  }

  if (request.method === "GET" && url.pathname.startsWith("/v1/runner-pools/")) {
    requireBearerToken(request, adminToken, "runner pool management");
    const poolId = requireUuidString(
      decodePathParam(url.pathname.slice("/v1/runner-pools/".length), "/runner_pool_id"),
      "/runner_pool_id",
    );
    const pool = await runners.getPool(poolId);
    if (!pool) {
      throw new HttpError(404, "runner_pool_not_found", "Runner pool not found");
    }
    return jsonResponse(poolToWire(pool));
  }

  if (request.method === "POST" && url.pathname.startsWith("/v1/runner-pools/") && url.pathname.endsWith("/drain")) {
    requireBearerToken(request, adminToken, "runner pool management");
    const poolId = requireUuidString(
      decodePathParam(url.pathname.slice("/v1/runner-pools/".length, -"/drain".length), "/runner_pool_id"),
      "/runner_pool_id",
    );
    const pool = await runners.setPoolStatus({ poolId, status: "draining" });
    return jsonResponse(poolToWire(pool));
  }

  if (request.method === "POST" && url.pathname.startsWith("/v1/runner-pools/") && url.pathname.endsWith("/disable")) {
    requireBearerToken(request, adminToken, "runner pool management");
    const poolId = requireUuidString(
      decodePathParam(url.pathname.slice("/v1/runner-pools/".length, -"/disable".length), "/runner_pool_id"),
      "/runner_pool_id",
    );
    const pool = await runners.setPoolStatus({ poolId, status: "disabled" });
    return jsonResponse(poolToWire(pool));
  }

  if (request.method === "POST" && url.pathname === "/v1/runner-instances/register") {
    requireBearerToken(request, workerToken, "runner instance registration");
    const body = await readJsonObject(request);
    const instance = await runners.registerInstance({
      runnerPoolId: requireUuidString(body.runner_pool_id, "/runner_pool_id"),
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
    requireBearerToken(request, adminToken, "runner instance management");
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
    name: publicBoundaryText(pool.name),
    status: pool.status,
    capabilities: publicStructuredJsonObject(pool.capabilities as unknown as JsonObject),
    metadata: publicBoundaryJsonObject(pool.metadata) ?? {},
    created_at: pool.created_at,
    updated_at: pool.updated_at,
  };
}

function instanceToWire(instance: RunnerInstanceRecord): JsonObject {
  return {
    runner_instance_id: instance.runner_instance_id,
    runner_pool_id: instance.runner_pool_id,
    instance_name: publicBoundaryText(instance.instance_name),
    status: instance.status,
    capabilities: publicStructuredJsonObject(instance.capabilities as unknown as JsonObject),
    metadata: publicBoundaryJsonObject(instance.metadata) ?? {},
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
    provider: publicBoundaryText(request.provider),
    provider_instance_id: request.provider_instance_id === null ? null : publicBoundaryText(request.provider_instance_id),
    instance_name: request.instance_name === null ? null : publicBoundaryText(request.instance_name),
    runner_instance_id: request.runner_instance_id,
    requirements: publicStructuredJsonObject(request.requirements),
    metadata: publicBoundaryJsonObject(request.metadata) ?? {},
    error_message: request.error_message === null ? null : publicBoundaryText(request.error_message),
    created_at: request.created_at,
    updated_at: request.updated_at,
  };
}

function publicStructuredJsonObject(value: JsonObject): JsonObject {
  return publicStructuredValue(value) as JsonObject;
}

function publicStructuredValue(value: JsonValue): JsonValue {
  if (typeof value === "string") {
    return publicBoundaryText(value);
  }
  if (Array.isArray(value)) {
    return value.map(publicStructuredValue);
  }
  if (isRecord(value)) {
    return Object.fromEntries(
      Object.entries(value).map(([key, child]) => [key, publicStructuredValue(child)]),
    ) as JsonObject;
  }
  return value;
}

function limitFromUrl(url: URL): number {
  return queryIntegerParam(url, "limit", { defaultValue: 100, min: 1, max: 500 });
}

function capabilitiesFromBody(value: unknown): WorkerCapabilities {
  if (!isRecord(value)) {
    return { executors: [], resources: [] };
  }
  return {
    executors: runnerExecutors(value.executors, "/capabilities/executors"),
    resources: capabilityStringArray(value.resources, "/capabilities/resources"),
    ...(value.arch !== undefined ? { arch: cloudArch(value.arch, "/capabilities/arch") } : {}),
    ...(value.cpu_count !== undefined ? { cpu_count: requirePositiveInt(value.cpu_count, "/capabilities/cpu_count") } : {}),
    ...(value.memory_mb !== undefined ? { memory_mb: requirePositiveInt(value.memory_mb, "/capabilities/memory_mb") } : {}),
    ...(value.disk_mb !== undefined ? { disk_mb: requirePositiveInt(value.disk_mb, "/capabilities/disk_mb") } : {}),
    ...(value.isolation !== undefined ? { isolation: runnerIsolation(value.isolation, "/capabilities/isolation") } : {}),
  };
}

function capabilityStringArray(value: unknown, pointer: string): string[] {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new HttpError(400, "unsupported_runner_capability", `${pointer} must be an array`);
  }
  return value.map((item, idx) => {
    if (typeof item !== "string" || item.trim().length === 0) {
      throw new HttpError(400, "unsupported_runner_capability", `${pointer}/${idx} must be a non-empty string`);
    }
    return item.trim();
  });
}

function runnerExecutors(value: unknown, pointer: string): string[] {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new HttpError(400, "unsupported_runner_capability", `${pointer} must be an array`);
  }
  const executors = value.map((item, idx) => {
    if (typeof item !== "string" || item.trim().length === 0) {
      throw new HttpError(400, "unsupported_runner_capability", `${pointer}/${idx} must be a non-empty string`);
    }
    return runnerExecutor(item, `${pointer}/${idx}`);
  });
  return [...new Set(executors)].sort();
}

function runnerExecutor(value: string, pointer: string): WorkerCapabilities["executors"][number] {
  switch (value.trim().toLowerCase()) {
    case "runner-docker":
    case "runner_docker":
    case "local-docker":
    case "local_docker":
      return "runner-docker";
    case "modal":
      return "modal";
    default:
      throw new HttpError(
        400,
        "unsupported_runner_capability",
        `${pointer} '${value}' is not supported; use runner-docker or modal`,
      );
  }
}

function positiveInt(value: unknown): number | null {
  if (typeof value === "number" && Number.isSafeInteger(value) && value > 0 && value <= MAX_RUNNER_CAPABILITY_INT) {
    return value;
  }
  if (typeof value === "string" && /^\d+$/.test(value)) {
    const parsed = Number.parseInt(value, 10);
    return Number.isSafeInteger(parsed) && parsed > 0 && parsed <= MAX_RUNNER_CAPABILITY_INT ? parsed : null;
  }
  return null;
}

function requirePositiveInt(value: unknown, pointer: string): number {
  const parsed = positiveInt(value);
  if (parsed !== null) {
    return parsed;
  }
  throw new HttpError(400, "unsupported_runner_capability", `${pointer} must be a positive integer`);
}

function optionalPositiveInt(value: unknown, pointer: string, defaultValue: number): number {
  if (value === undefined || value === null) {
    return defaultValue;
  }
  const parsed = positiveInt(value);
  if (parsed !== null) {
    return parsed;
  }
  throw new HttpError(400, "invalid_request", `${pointer} must be a positive integer`);
}

function requireUuidString(value: unknown, pointer: string): string {
  if (typeof value !== "string") {
    throw new HttpError(400, "invalid_request", `${pointer} must be a UUID`);
  }
  const normalized = value.trim().toLowerCase();
  if (!UUID_PATTERN.test(normalized)) {
    throw new HttpError(400, "invalid_request", `${pointer} must be a UUID`);
  }
  return normalized;
}

function optionalUuidString(value: unknown, pointer: string): string | null {
  if (value === undefined || value === null) {
    return null;
  }
  return requireUuidString(value, pointer);
}

function cloudArch(value: unknown, pointer: string): string {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new HttpError(400, "unsupported_runner_capability", `${pointer} must be x86_64 or arm64`);
  }
  switch (value.trim().toLowerCase()) {
    case "x64":
    case "amd64":
    case "x86_64":
      return "x86_64";
    case "arm64":
    case "aarch64":
      return "arm64";
    default:
      throw new HttpError(400, "unsupported_runner_capability", `Unsupported runner architecture '${value}'`);
  }
}

function runnerIsolation(value: unknown, pointer: string): string[] {
  const modes = capabilityStringArray(value, pointer);
  for (const mode of modes) {
    if (mode !== "reusable_vm" && mode !== "single_use_vm") {
      throw new HttpError(400, "unsupported_runner_capability", `Unsupported runner isolation mode '${mode}'`);
    }
  }
  return [...new Set(modes)].sort();
}

function runnerInstancePath(pathname: string, suffix: string): boolean {
  return pathname.startsWith("/v1/runner-instances/") && pathname.endsWith(suffix);
}

function runnerInstanceIdFromPath(pathname: string, suffix: string): string {
  return requireUuidString(
    decodePathParam(pathname.slice("/v1/runner-instances/".length, -suffix.length), "/runner_instance_id"),
    "/runner_instance_id",
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
