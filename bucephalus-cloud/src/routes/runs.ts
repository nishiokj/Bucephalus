import { authOwnerKey, type AuthContext } from "../auth";
import { HttpError, jsonResponse, optionalQueryInteger, optionalString, queryInteger, readJsonObject, readRequestBytes, requireBearerToken, requireString } from "../http";
import { logError, newTraceContext } from "../logging";
import { putRuntimeObject, readStoredObject } from "../objectStorage";
import { sha256Digest, type JsonObject, type JsonValue } from "../primitives";
import {
  RuntimeRepository,
  type RuntimeEventFilter,
  type RuntimeEventRecord,
  type RuntimeEventRowInsert,
  type RuntimeAttemptObjectContentRecord,
  type RuntimeAttemptObjectRecord,
  type RuntimeResourceFilter,
  type RuntimeResourceListInput,
  type RuntimeResourceMetricsListInput,
  type RuntimeResourceWatchInput,
  type WorkerLifecycleEventRecord,
} from "../runtime/repository";
import { RunnerRepository, type RunnerInstanceRecord, type RunnerPoolRecord } from "../runners/repository";
import {
  optionalJsonObject,
  PackageRepository,
  requireStringMap,
  RunRepository,
  type CloudRunRecord,
  type PackageArtifactRecord,
  type RunAttemptRecord,
  type RunEventRecord,
  type RunNetworkMode,
  type RunRequirements,
  type WorkerCapabilities,
} from "../packages/repository";
import {
  allowsControlPlaneSecretRefs,
  controlPlaneEnvNameViolation,
  controlPlaneSecretIdViolation,
  controlPlaneSecretRefViolation,
} from "../secrets/policy";
import type { CloudSecretRepository } from "../secrets/repository";
import { SECRET_NAME_PATTERN } from "../secrets/store";

const HOSTED_SECRET_REF_PREFIX = "bucephalus://";
const SHA256_DIGEST_PATTERN = /^sha256:[0-9a-f]{64}$/;

interface CloudSecretRequirement {
  id: string;
  target: string;
  required_for_variants: string[];
}

type PendingReason = "no_matching_runner" | "waiting_for_capacity";

interface RunWireEnrichment {
  experiment_name: string | null;
  trials_completed: number | null;
  trials_total: number | null;
  pending_reason: PendingReason | null;
}

interface RuntimeResourceRef {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: string;
  name: string;
  uid?: string;
}

type RuntimeResourceRoute =
  | { kind: "resource-list"; runId: string }
  | { kind: "resource-health"; runId: string }
  | { kind: "resource-watch"; runId: string }
  | { kind: "resource-metrics-list"; runId: string }
  | { kind: "resource-item"; runId: string; resourceKind: string; resourceName: string }
  | { kind: "resource-status"; runId: string; resourceKind: string; resourceName: string }
  | { kind: "resource-events"; runId: string; resourceKind: string; resourceName: string }
  | { kind: "resource-logs"; runId: string; resourceKind: string; resourceName: string }
  | { kind: "resource-metrics"; runId: string; resourceKind: string; resourceName: string }
  | { kind: "resource-content"; runId: string; resourceKind: string; resourceName: string }
  | { kind: "resource-operation"; runId: string; resourceKind: string; resourceName: string; operation: string }
  | { kind: "resource-action"; runId: string; resourceKind: string; resourceName: string; action: string }
  | { kind: "resource-port-forward"; runId: string; resourceKind: string; resourceName: string }
  | { kind: "resource-exec"; runId: string; resourceKind: string; resourceName: string }
  | { kind: "api-resources"; runId: string }
  | { kind: "api-resource"; runId: string; resourceKind: string }
  | { kind: "inspect"; runId: string };

export async function handleRunRoute(
  request: Request,
  url: URL,
  packages: PackageRepository,
  runs: RunRepository,
  runtime: RuntimeRepository,
  runners: RunnerRepository,
  workerToken: string,
  auth?: AuthContext | null,
  secrets?: CloudSecretRepository,
): Promise<Response | null> {
  const ownerKey = authOwnerKey(auth);
  if (request.method === "POST" && url.pathname === "/v1/worker/runs/claim") {
    requireBearerToken(request, workerToken, "worker run claim");
    return claimRun(request, runs);
  }

  if (request.method === "POST" && workerAttemptPath(url.pathname, "/heartbeat")) {
    return heartbeatAttempt(request, url, runs);
  }

  if (request.method === "POST" && workerAttemptPath(url.pathname, "/events")) {
    return appendRunEvent(request, url, runs);
  }

  if (request.method === "POST" && workerAttemptPath(url.pathname, "/runtime/event-rows")) {
    return ingestRuntimeEventRows(request, url, runs, runtime);
  }

  if (request.method === "POST" && workerAttemptPath(url.pathname, "/runtime/artifacts")) {
    return uploadRuntimeArtifact(request, url, runs, runtime);
  }

  if (request.method === "POST" && workerAttemptPath(url.pathname, "/complete")) {
    return completeAttempt(request, url, runs);
  }

  if (request.method === "POST" && workerAttemptPath(url.pathname, "/fail")) {
    return failAttempt(request, url, runs);
  }

  const workerPortForwardRoute = workerPortForwardPath(url.pathname);
  if (workerPortForwardRoute) {
    if (request.method === "GET" && workerPortForwardRoute.kind === "list") {
      return workerListPortForwards(request, workerPortForwardRoute, runs, runtime);
    }
    if (request.method === "POST" && workerPortForwardRoute.kind === "update") {
      return workerUpdatePortForward(request, workerPortForwardRoute, runs, runtime);
    }
    throw new HttpError(405, "worker_runtime_method_not_allowed", "Worker runtime endpoint does not support this method", {
      method: request.method,
      path: url.pathname,
    });
  }

  const workerExecRoute = workerExecPath(url.pathname);
  if (workerExecRoute) {
    if (request.method === "GET" && workerExecRoute.kind === "list") {
      return workerListExecRequests(request, workerExecRoute, runs, runtime);
    }
    if (request.method === "POST" && workerExecRoute.kind === "update") {
      return workerUpdateExecRequest(request, workerExecRoute, runs, runtime);
    }
    throw new HttpError(405, "worker_runtime_method_not_allowed", "Worker runtime endpoint does not support this method", {
      method: request.method,
      path: url.pathname,
    });
  }

  if (isWorkerRuntimeSubpath(url.pathname)) {
    throw new HttpError(404, "worker_runtime_endpoint_not_found", "Worker runtime endpoint not found; use resource-native worker runtime paths", {
      path: url.pathname,
      resource_apis: [
        "/v1/worker/run-attempts/{attempt_id}/runtime/resources/PortForward",
        "/v1/worker/run-attempts/{attempt_id}/runtime/resources/Exec",
      ],
    });
  }

  if (request.method === "POST" && url.pathname === "/v1/worker/runs/expire-leases") {
    requireBearerToken(request, workerToken, "worker lease expiration");
    return expireLeases(runs, runtime);
  }

  if (request.method === "GET" && packageContentPath(url.pathname)) {
    const digest = requirePackageDigest(packageDigestFromContentPath(url.pathname), "/package_digest");
    const attempt = await requireAttemptToken(request, runs, {
      attemptId: requireHeader(request, "x-bucephalus-attempt-id", "package content download"),
      packageDigest: digest,
    });
    const artifact = await packages.getArtifact(digest, attempt.ownerKey ?? undefined);
    if (!artifact) {
      throw new HttpError(404, "package_not_found", "Package artifact not found");
    }
    if (artifact.status !== "accepted" || !artifact.storage_path) {
      throw new HttpError(409, "package_content_unavailable", "Package artifact content is unavailable");
    }
    const bytes = await readStoredObject(artifact.storage_path);
    return new Response(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer, {
      headers: {
        "content-type": artifact.media_type ?? "application/octet-stream",
        "content-length": String(bytes.byteLength),
        "x-bucephalus-package-digest": artifact.package_digest,
      },
    });
  }

  if (request.method === "GET" && url.pathname === "/v1/packages") {
    const limit = limitFromUrl(url);
    const includeConfig = optionalBooleanFromUrl(url, "include_config", true);
    const artifacts = await packages.listArtifacts({ limit, ownerKey });
    return jsonResponse({ packages: artifacts.map((artifact) => packageToWire(artifact, { includeConfig })) });
  }

  if (request.method === "GET" && packagePath(url.pathname)) {
    const digest = requirePackageDigest(
      decodeURIComponent(url.pathname.slice("/v1/packages/".length)),
      "/package_digest",
    );
    const artifact = await packages.getArtifact(digest, ownerKey);
    if (!artifact) {
      throw new HttpError(404, "package_not_found", "Package artifact not found");
    }
    return jsonResponse(packageToWire(artifact, { includeConfig: true }));
  }

  if (request.method === "POST" && url.pathname === "/v1/runs") {
    return createRun(request, packages, runs, runners, ownerKey, secrets);
  }

  if (request.method === "GET" && url.pathname === "/v1/runs") {
    const limit = limitFromUrl(url);
    const packageDigest = optionalPackageDigestFromUrl(url);
    const includeRuntime = optionalBooleanFromUrl(url, "include_runtime", true);
    const includeConfig = optionalBooleanFromUrl(url, "include_config", true);
    const records = await runs.listRuns({ limit, ownerKey, packageDigest });
    const enrichment = await enrichRuns(records, packages, runtime, runners, ownerKey, { includeRuntime });
    return jsonResponse({ runs: records.map((run) => runToWire(run, enrichment.get(run.run_id), { includeConfig, includeRuntime })) });
  }

  const runtimeResourceRoute = runtimeResourcePath(url.pathname);
  if (runtimeResourceRoute) {
    const run = await runs.getRun(runtimeResourceRoute.runId, ownerKey);
    if (!run) {
      throw new HttpError(404, "run_not_found", "Run not found");
    }
    if (request.method === "GET") {
      return runtimeResourceResponse(url, runtimeResourceRoute, run, runtime, ownerKey ?? null);
    }
    if (request.method === "DELETE" && runtimeResourceRoute.kind === "resource-item") {
      const body = await optionalJsonBody(request);
      const resourceVersion = requireRuntimeResourceVersionPrecondition(body, "delete");
      return jsonResponse(await runtime.cancelRuntimeAccessResource({
        run,
        resourceKind: runtimeResourceRoute.resourceKind,
        resourceName: runtimeResourceRoute.resourceName,
        resourceVersion,
        requester: ownerKey ?? null,
        reason: body ? optionalString(body.reason, "/reason") : null,
      }));
    }
    if (request.method === "POST" && runtimeResourceRoute.kind === "resource-action") {
      const body = await optionalJsonBody(request);
      const resourceVersion = requireRuntimeResourceVersionPrecondition(body, runtimeResourceRoute.action);
      const input = {
        run,
        resourceKind: runtimeResourceRoute.resourceKind,
        resourceName: runtimeResourceRoute.resourceName,
        resourceVersion,
        requester: ownerKey ?? null,
        reason: body ? optionalString(body.reason, "/reason") : null,
      };
      switch (runtimeResourceRoute.action) {
        case "cordon":
          return jsonResponse(await runtime.cordonRunnerInstanceResource(input));
        case "drain":
          return jsonResponse(await runtime.drainRunnerInstanceResource(input));
        case "uncordon":
          return jsonResponse(await runtime.uncordonRunnerInstanceResource(input));
        case "cancel":
          return jsonResponse(await runtime.cancelRuntimeAccessResource(input));
        case "complete":
          return jsonResponse(await runtime.completeRuntimeAccessResource(input));
        default:
          throw new HttpError(404, "runtime_resource_action_not_found", "Runtime resource action not found", {
            action: runtimeResourceRoute.action,
          });
      }
    }
    if (request.method === "POST" && runtimeResourceRoute.kind === "resource-port-forward") {
      const body = await readJsonObject(request);
      assertRuntimeAccessBodyHasNoTarget(body);
      const portForward = await runtime.createPortForwardRequest({
        run,
        targetPort: requirePort(body.target_port, "/target_port"),
        localPort: optionalPort(body.local_port, "/local_port"),
        resourceKind: runtimeResourceRoute.resourceKind,
        resourceName: runtimeResourceRoute.resourceName,
        resourceVersion: requireRuntimeResourceVersionPrecondition(body, "port-forward"),
        protocol: optionalString(body.protocol, "/protocol"),
        ttlSeconds: optionalPositiveInt(body.ttl_seconds, "/ttl_seconds"),
        requester: ownerKey ?? null,
        reason: optionalString(body.reason, "/reason"),
      });
      return jsonResponse({ resource: runtime.runtimeAccessRequestResource(run, portForward) }, { status: 201 });
    }
    if (request.method === "POST" && runtimeResourceRoute.kind === "resource-exec") {
      const body = await readJsonObject(request);
      assertRuntimeAccessBodyHasNoTarget(body);
      const exec = await runtime.createExecRequest({
        run,
        command: requireStringArray(body.command, "/command"),
        resourceKind: runtimeResourceRoute.resourceKind,
        resourceName: runtimeResourceRoute.resourceName,
        resourceVersion: requireRuntimeResourceVersionPrecondition(body, "exec"),
        ttlSeconds: optionalPositiveInt(body.ttl_seconds, "/ttl_seconds"),
        requester: ownerKey ?? null,
        reason: optionalString(body.reason, "/reason"),
      });
      return jsonResponse({ resource: runtime.runtimeAccessRequestResource(run, exec) }, { status: 201 });
    }
    throw new HttpError(405, "runtime_method_not_allowed", "Runtime resource endpoint does not support this method", {
      method: request.method,
      path: url.pathname,
    });
  }

  const runtimeRoute = runtimePath(url.pathname);
  if (request.method === "GET" && runtimeRoute) {
    const run = await runs.getRun(runtimeRoute.runId, ownerKey);
    if (!run) {
      throw new HttpError(404, "run_not_found", "Run not found");
    }
    const limit = limitFromUrl(url);
    const afterRowSeq = runtimeEventAfterRowSeqFromUrl(url);
    const [trialRows, workerEvents] = await Promise.all([
      runtime.eventRows(runtimeRoute.runId, {
        limit,
        afterRowSeq: afterRowSeq ?? undefined,
        sources: ["runtime.event_rows", "worker_runtime_snapshot"],
      }),
      runtime.workerLifecycleEvents(runtimeRoute.runId, { limit, afterRowSeq: afterRowSeq ?? undefined }),
    ]);
    return jsonResponse(runtimeEventListToWire(runtimeRoute.runId, [], trialRows, workerEvents, url, limit, afterRowSeq));
  }

  if (request.method === "GET" && url.pathname.startsWith("/v1/runs/")) {
    const runIdPath = url.pathname.slice("/v1/runs/".length);
    if (!runIdPath || runIdPath.includes("/")) {
      return null;
    }
    const runId = decodeURIComponent(runIdPath);
    const run = await runs.getRun(runId, ownerKey);
    if (!run) {
      throw new HttpError(404, "run_not_found", "Run not found");
    }
    const [attempts, enrichment] = await Promise.all([
      runs.listAttempts(runId),
      enrichRuns([run], packages, runtime, runners, ownerKey),
    ]);
    return jsonResponse({
      ...runToWire(run, enrichment.get(run.run_id)),
      attempts: attempts.map(attemptToWire),
    });
  }

  return null;
}

function limitFromUrl(url: URL): number {
  return queryInteger(url, "limit", { defaultValue: 50, min: 1, max: 1000 });
}

function optionalIntFromUrl(url: URL, key: string): number | undefined {
  return optionalQueryInteger(url, key, { min: 0 });
}

function optionalPackageDigestFromUrl(url: URL): string | undefined {
  const value = optionalString(url.searchParams.get("package_digest"), "/package_digest");
  return value === null ? undefined : requirePackageDigest(value, "/package_digest");
}

function optionalBooleanFromUrl(url: URL, key: string, defaultValue: boolean): boolean {
  const value = optionalString(url.searchParams.get(key), `/${key}`);
  if (value === null) return defaultValue;
  switch (value.trim().toLowerCase()) {
    case "1":
    case "true":
    case "yes":
    case "on":
      return true;
    case "0":
    case "false":
    case "no":
    case "off":
      return false;
    default:
      throw new HttpError(400, "invalid_request", `/${key} must be a boolean`);
  }
}

function runtimePath(pathname: string):
  | { kind: "events"; runId: string }
  | null {
  const prefix = "/v1/runs/";
  if (!pathname.startsWith(prefix)) {
    return null;
  }
  const parts = pathname.slice(prefix.length).split("/").map(decodeURIComponent);
  if (parts.length === 3 && parts[1] === "runtime" && parts[2] === "events") {
    return { kind: "events", runId: parts[0] ?? "" };
  }
  return null;
}

function runtimeResourcePath(pathname: string): RuntimeResourceRoute | null {
  const prefix = "/v1/runs/";
  if (!pathname.startsWith(prefix)) {
    return null;
  }
  const parts = pathname.slice(prefix.length).split("/").map(decodeURIComponent);
  const runId = parts[0] ?? "";
  if (parts[1] !== "runtime") {
    return null;
  }
  if (parts.length === 3 && parts[2] === "inspect") {
    return { kind: "inspect", runId };
  }
  if (parts.length === 3 && parts[2] === "api-resources") {
    return { kind: "api-resources", runId };
  }
  if (parts.length === 4 && parts[2] === "api-resources") {
    return { kind: "api-resource", runId, resourceKind: parts[3] ?? "" };
  }
  if (parts[2] !== "resources") {
    return null;
  }
  if (parts.length === 3) {
    return { kind: "resource-list", runId };
  }
  if (parts.length === 4 && parts[3] === "health") {
    return { kind: "resource-health", runId };
  }
  if (parts.length === 4 && parts[3] === "watch") {
    return { kind: "resource-watch", runId };
  }
  if (parts.length === 4 && parts[3] === "metrics") {
    return { kind: "resource-metrics-list", runId };
  }
  const resourceKind = parts[3] ?? "";
  const resourceName = parts[4] ?? "";
  if (!resourceKind || !resourceName) {
    return null;
  }
  if (parts.length === 5) {
    return { kind: "resource-item", runId, resourceKind, resourceName };
  }
  if (parts.length === 7 && parts[5] === "actions") {
    return { kind: "resource-action", runId, resourceKind, resourceName, action: parts[6] ?? "" };
  }
  if (parts.length === 6) {
    switch (parts[5]) {
      case "status":
        return { kind: "resource-status", runId, resourceKind, resourceName };
      case "events":
        return { kind: "resource-events", runId, resourceKind, resourceName };
      case "logs":
        return { kind: "resource-logs", runId, resourceKind, resourceName };
      case "metrics":
        return { kind: "resource-metrics", runId, resourceKind, resourceName };
      case "content":
        return { kind: "resource-content", runId, resourceKind, resourceName };
      case "port-forward":
        return { kind: "resource-port-forward", runId, resourceKind, resourceName };
      case "exec":
        return { kind: "resource-exec", runId, resourceKind, resourceName };
      default:
        return null;
    }
  }
  if (parts.length === 7 && parts[5] === "operations") {
    return { kind: "resource-operation", runId, resourceKind, resourceName, operation: parts[6] ?? "" };
  }
  return null;
}

async function claimRun(request: Request, runs: RunRepository): Promise<Response> {
  const body = await readJsonObject(request);
  const runnerInstanceId = requireString(body.runner_instance_id, "/runner_instance_id");
  const leaseSeconds = leaseSecondsFromBody(body.lease_seconds);
  const claim = await runs.claimNextRun({ runnerInstanceId, leaseSeconds });
  return jsonResponse(claim ? claimToWire(claim) : { claimed: false });
}

async function heartbeatAttempt(
  request: Request,
  url: URL,
  runs: RunRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const attemptId = attemptIdFromWorkerPath(url.pathname, "/heartbeat");
  const runnerInstanceId = requireString(body.runner_instance_id, "/runner_instance_id");
  await requireAttemptToken(request, runs, { attemptId, runnerInstanceId });
  const attempt = await runs.heartbeatAttempt({
    attemptId,
    runnerInstanceId,
    leaseSeconds: leaseSecondsFromBody(body.lease_seconds),
  });
  return jsonResponse({ attempt: attemptToWire(attempt) });
}

async function appendRunEvent(
  request: Request,
  url: URL,
  runs: RunRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const attemptId = attemptIdFromWorkerPath(url.pathname, "/events");
  const runnerInstanceId = requireString(body.runner_instance_id, "/runner_instance_id");
  await requireAttemptToken(request, runs, { attemptId, runnerInstanceId });
  const event = await runs.appendRunEvent({
    attemptId,
    runnerInstanceId,
    eventType: requireString(body.event_type, "/event_type"),
    payload: optionalJsonObject(body.payload as JsonValue | undefined, "/payload"),
  });
  return jsonResponse({ event: eventToWire(event) }, { status: 201 });
}

const INGEST_MAX_ROWS_PER_BATCH = 500;
const INGEST_MAX_ROW_PAYLOAD_BYTES = 64 * 1024;
const CORE_RUN_ID_PATTERN = /^run_[A-Za-z0-9_.-]+$/;
const RUNTIME_ARTIFACT_ROLE_PATTERN = /^[a-z][a-z0-9_]{0,63}$/;
const DEFAULT_MAX_RUNTIME_ARTIFACT_BYTES = 16 * 1024 * 1024;

async function ingestRuntimeEventRows(
  request: Request,
  url: URL,
  runs: RunRepository,
  runtime: RuntimeRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const attemptId = attemptIdFromWorkerPath(url.pathname, "/runtime/event-rows");
  const runnerInstanceId = requireString(body.runner_instance_id, "/runner_instance_id");
  const { runId } = await requireAttemptToken(request, runs, { attemptId, runnerInstanceId });
  if (!Array.isArray(body.rows) || body.rows.length === 0) {
    throw new HttpError(400, "invalid_rows", "/rows must be a non-empty array");
  }
  if (body.rows.length > INGEST_MAX_ROWS_PER_BATCH) {
    throw new HttpError(
      400,
      "too_many_rows",
      `/rows must contain at most ${INGEST_MAX_ROWS_PER_BATCH} rows per batch`,
    );
  }
  const rows = body.rows.map((row, index) =>
    runtimeEventRowFromWire(row, `/rows/${index}`, { cloudRunId: runId, attemptId }),
  );
  const upserted = await runtime.upsertEventRows(rows);
  return jsonResponse({ received: rows.length, upserted });
}

async function uploadRuntimeArtifact(
  request: Request,
  url: URL,
  runs: RunRepository,
  runtime: RuntimeRepository,
): Promise<Response> {
  const attemptId = attemptIdFromWorkerPath(url.pathname, "/runtime/artifacts");
  const runnerInstanceId = requireHeader(request, "x-bucephalus-runner-instance-id", "runtime artifact upload");
  const { runId } = await requireAttemptToken(request, runs, { attemptId, runnerInstanceId });
  const coreRunId = requireCoreRunId(requireHeader(request, "x-bucephalus-core-run-id", "runtime artifact upload"), "/headers/x-bucephalus-core-run-id");
  const trialId = requireArtifactToken(requireHeader(request, "x-bucephalus-trial-id", "runtime artifact upload"), "/headers/x-bucephalus-trial-id");
  const role = requireArtifactRole(requireHeader(request, "x-bucephalus-artifact-role", "runtime artifact upload"), "/headers/x-bucephalus-artifact-role");
  const scheduleIdx = nonNegativeHeaderInt(request, "x-bucephalus-schedule-idx", "runtime artifact upload");
  const attempt = nonNegativeHeaderInt(request, "x-bucephalus-trial-attempt", "runtime artifact upload");
  const mediaType = request.headers.get("content-type")?.trim() || "application/octet-stream";
  const relativePath = optionalHeader(request, "x-bucephalus-artifact-relative-path");
  const bytes = await readRequestBytes(request, maxRuntimeArtifactBytes(), "Runtime artifact body");
  const digest = sha256Digest(bytes);
  const objectRef = runtimeObjectRef({
    coreRunId,
    trialId,
    attempt,
    role,
    digest,
  });
  const storagePath = await putRuntimeObject({
    cloudRunId: runId,
    attemptId,
    coreRunId,
    trialId,
    trialAttempt: attempt,
    role,
    bytes,
    mediaType,
  });
  const metadata = {
    source: "worker_runtime_artifact_upload",
    cloud_run_id: runId,
    attempt_id: attemptId,
    runner_instance_id: runnerInstanceId,
    relative_path: relativePath,
    media_type: mediaType,
    byte_size: bytes.byteLength,
    sha256: digest,
  };
  const record = await runtime.upsertAttemptObjectContent({
    core_run_id: coreRunId,
    trial_id: trialId,
    schedule_idx: scheduleIdx,
    attempt,
    role,
    object_ref: objectRef,
    storage_path: storagePath,
    media_type: mediaType,
    byte_size: bytes.byteLength,
    sha256: digest,
    relative_path: relativePath ?? null,
    metadata,
    recorded_at_ms: Date.now(),
  });
  return jsonResponse({ artifact: runtimeArtifactToWire(record) }, { status: 201 });
}

function runtimeArtifactToWire(record: RuntimeAttemptObjectContentRecord): JsonObject {
  return {
    core_run_id: record.core_run_id,
    trial_id: record.trial_id,
    schedule_idx: record.schedule_idx,
    attempt: record.attempt,
    role: record.role,
    object_ref: record.object_ref,
    content_available: true,
    media_type: record.media_type,
    byte_size: record.byte_size,
    sha256: record.sha256,
    relative_path: record.relative_path,
    recorded_at_ms: record.recorded_at_ms,
  };
}

async function runtimeResourceResponse(
  url: URL,
  route: RuntimeResourceRoute,
  run: CloudRunRecord,
  runtime: RuntimeRepository,
  requester?: string | null,
): Promise<Response> {
  switch (route.kind) {
    case "resource-list":
      return jsonResponse(await runtime.resources(route.runId, run, {
        ...runtimeResourceListInputFromUrl(url),
        requester,
      }));
    case "resource-health":
      return jsonResponse(await runtime.resourceHealth(route.runId, run, {
        ...runtimeResourceFilterFromUrl(url),
        requester,
      }));
    case "resource-watch":
      return jsonResponse(await runtime.watchResources(route.runId, run, {
        ...runtimeResourceWatchInputFromUrl(url),
        requester,
      }));
    case "resource-metrics-list":
      return jsonResponse(await runtime.resourceMetricsList(route.runId, run, {
        ...runtimeResourceMetricsListInputFromUrl(url),
        requester,
      }));
    case "api-resources":
      return jsonResponse(await runtime.apiResources(route.runId, run, { requester }));
    case "api-resource":
      return jsonResponse(await runtime.apiResource(route.runId, run, route.resourceKind, { requester }));
    case "inspect":
      return jsonResponse(await runtime.inspectBundle(route.runId, run, {
        ...runtimeInspectBundleInputFromUrl(url),
        requester,
      }));
    case "resource-item": {
      if (url.searchParams.get("view") === "resource") {
        return jsonResponse(await runtime.getResource(route.runId, run, {
          kind: route.resourceKind,
          name: route.resourceName,
          requester,
        }));
      }
      return jsonResponse(await runtime.describeResource(route.runId, run, {
        kind: route.resourceKind,
        name: route.resourceName,
        eventLimit: limitFromUrl(url),
        requester,
      }));
    }
    case "resource-status":
      return jsonResponse(await runtime.resourceStatus(route.runId, run, {
        kind: route.resourceKind,
        name: route.resourceName,
        requester,
      }));
    case "resource-events":
      return jsonResponse(await runtime.resourceEvents(route.runId, run, {
        kind: route.resourceKind,
        name: route.resourceName,
        limit: limitFromUrl(url),
        afterRowSeq: optionalIntFromUrl(url, "after_row_seq"),
        continueToken: optionalString(url.searchParams.get("continue"), "/continue"),
        filter: runtimeEventFilterFromUrl(url),
        requester,
      }));
    case "resource-logs":
      return runtimeResourceLogsResponse(route.runId, run, runtime, route, url, requester);
    case "resource-content":
      return runtimeResourceContentResponse(route.runId, run, runtime, route, requester);
    case "resource-metrics":
      return jsonResponse(await runtime.resourceMetrics(route.runId, run, {
        kind: route.resourceKind,
        name: route.resourceName,
        requester,
      }));
    case "resource-operation":
      return jsonResponse(await runtime.reviewResourceOperation(route.runId, run, {
        kind: route.resourceKind,
        name: route.resourceName,
        operation: route.operation,
        requester,
      }));
    case "resource-action":
    case "resource-port-forward":
    case "resource-exec":
      throw new HttpError(405, "runtime_method_not_allowed", "Runtime resource endpoint does not support this method", {
        method: "GET",
        resource_kind: route.resourceKind,
        resource_name: route.resourceName,
      });
  }
}

function runtimeResourceFilterFromUrl(url: URL): RuntimeResourceFilter {
  return {
    kinds: url.searchParams.getAll("kind").flatMap(splitCsv),
    categories: url.searchParams.getAll("category").flatMap(splitCsv),
    labelSelector: optionalString(url.searchParams.get("label_selector"), "/label_selector"),
    fieldSelector: optionalString(url.searchParams.get("field_selector"), "/field_selector"),
  };
}

function runtimeResourceListInputFromUrl(url: URL): RuntimeResourceListInput {
  return {
    ...runtimeResourceFilterFromUrl(url),
    limit: limitFromUrl(url),
    continueToken: optionalString(url.searchParams.get("continue"), "/continue"),
  };
}

function runtimeResourceMetricsListInputFromUrl(url: URL): RuntimeResourceMetricsListInput {
  return {
    ...runtimeResourceFilterFromUrl(url),
    limit: limitFromUrl(url),
    continueToken: optionalString(url.searchParams.get("continue"), "/continue"),
  };
}

function runtimeResourceWatchInputFromUrl(url: URL): RuntimeResourceWatchInput {
  return {
    filter: runtimeResourceFilterFromUrl(url),
    resourceVersion: optionalString(url.searchParams.get("resource_version"), "/resource_version"),
    knownResourceVersions: runtimeKnownResourceVersionsFromUrl(url),
    allowBookmarks: optionalBooleanFromUrl(url, "allow_bookmarks", false),
  };
}

function runtimeInspectBundleInputFromUrl(url: URL): RuntimeResourceFilter & { eventLimit: number } {
  return {
    ...runtimeResourceFilterFromUrl(url),
    eventLimit: queryInteger(url, "event_limit", { defaultValue: 100, min: 1, max: 1000 }),
  };
}

function runtimeEventFilterFromUrl(url: URL): RuntimeEventFilter {
  return {
    eventTypes: url.searchParams.getAll("event_type").flatMap(splitCsv),
    sources: url.searchParams.getAll("source").flatMap(splitCsv),
    trialId: optionalString(url.searchParams.get("trial_id"), "/trial_id"),
    taskId: optionalString(url.searchParams.get("task_id"), "/task_id"),
  };
}

function runtimeEventAfterRowSeqFromUrl(url: URL): number | null {
  const afterRowSeq = optionalIntFromUrl(url, "after_row_seq");
  const continueToken = optionalString(url.searchParams.get("continue"), "/continue");
  if (afterRowSeq !== undefined && continueToken !== null) {
    throw new HttpError(400, "invalid_runtime_event_cursor", "Runtime event queries accept either after_row_seq or continue, not both");
  }
  if (continueToken !== null) {
    return runtimeEventContinueRowSeq(continueToken, "/continue");
  }
  return afterRowSeq ?? null;
}

function runtimeEventContinueRowSeq(token: string, pointer: string): number {
  const match = /^event-row-seq:(\d+)$/.exec(token.trim());
  if (!match) {
    throw new HttpError(400, "invalid_runtime_event_continue", `${pointer} must be formatted as event-row-seq:<row_seq>`);
  }
  const rowSeq = Number.parseInt(match[1] ?? "", 10);
  if (!Number.isSafeInteger(rowSeq) || rowSeq < 0) {
    throw new HttpError(400, "invalid_runtime_event_continue", `${pointer} must be formatted as event-row-seq:<row_seq>`);
  }
  return rowSeq;
}

function runtimeKnownResourceVersionsFromUrl(url: URL): Map<string, string> {
  const versions = new Map<string, string>();
  for (const raw of url.searchParams.getAll("known_resource").flatMap(splitCsv)) {
    const separator = raw.indexOf("=");
    if (separator <= 0 || separator === raw.length - 1) {
      throw new HttpError(400, "invalid_known_resource", "/known_resource must be formatted as Kind/name=resourceVersion");
    }
    const key = raw.slice(0, separator).trim();
    const version = raw.slice(separator + 1).trim();
    if (!key || !version) {
      throw new HttpError(400, "invalid_known_resource", "/known_resource must be formatted as Kind/name=resourceVersion");
    }
    versions.set(key, version);
  }
  return versions;
}

function runtimeEventListToWire(
  cloudRunId: string,
  coreRunIds: string[],
  trialRows: RuntimeEventRecord[],
  workerEvents: WorkerLifecycleEventRecord[],
  url: URL,
  limit: number,
  afterRowSeq: number | null,
): Record<string, unknown> {
  const filteredEvents = runtimeEventStreamToWire(trialRows, workerEvents, afterRowSeq)
    .filter((event) => runtimeEventMatchesFilters(event, url));
  const events = filteredEvents.slice(0, limit);
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "RuntimeEventList",
    cloud_run_id: cloudRunId,
    generated_at: new Date().toISOString(),
    event_filter: runtimeEventFilterToWire(url),
    metadata: runtimeEventMetadata(events, limit, afterRowSeq, filteredEvents.length > limit),
    core_run_ids: coreRunIds,
    events,
  };
}

function runtimeEventMetadata(events: JsonObject[], limit: number, afterRowSeq: number | null, hasMore: boolean): Record<string, unknown> {
  const next = runtimeEventNextAfterRowSeq(afterRowSeq, events);
  return {
    resourceVersion: runtimeEventContinueToken(next ?? afterRowSeq ?? 0),
    continue: hasMore && next !== null ? runtimeEventContinueToken(next) : null,
    remainingItemCount: hasMore ? null : 0,
    limit,
    returned: events.length,
    after_row_seq: afterRowSeq,
    next_after_row_seq: next,
  };
}

function runtimeEventNextAfterRowSeq(afterRowSeq: number | null, events: JsonObject[]): number | null {
  const observed = events
    .map((event) => numberField(event.row_seq) ?? numberField(event.seq))
    .filter((value): value is number => value !== null)
    .reduce<number | null>((max, value) => max === null ? value : Math.max(max, value), null);
  if (observed !== null) return Math.max(afterRowSeq ?? observed, observed);
  return afterRowSeq;
}

function runtimeEventContinueToken(rowSeq: number): string {
  return `event-row-seq:${rowSeq}`;
}

function runtimeEventFilterToWire(url: URL): Record<string, unknown> {
  return {
    event_types: url.searchParams.getAll("event_type").flatMap(splitCsv).sort(),
    sources: url.searchParams.getAll("source").flatMap(splitCsv).sort(),
    resource_kind: optionalString(url.searchParams.get("resource_kind"), "/resource_kind"),
    resource_name: optionalString(url.searchParams.get("resource_name"), "/resource_name"),
    trial_id: optionalString(url.searchParams.get("trial_id"), "/trial_id"),
    task_id: optionalString(url.searchParams.get("task_id"), "/task_id"),
  };
}

function runtimeEventMatchesFilters(event: JsonObject, url: URL): boolean {
  const types = url.searchParams.getAll("event_type").flatMap(splitCsv);
  const sources = url.searchParams.getAll("source").flatMap(splitCsv);
  if (types.length > 0 && !types.includes(String(event.event_type))) return false;
  if (sources.length > 0 && !sources.includes(String(event.source))) return false;
  const trialId = optionalString(url.searchParams.get("trial_id"), "/trial_id");
  if (trialId !== null && event.trial_id !== trialId) return false;
  const taskId = optionalString(url.searchParams.get("task_id"), "/task_id");
  if (taskId !== null && event.task_id !== taskId) return false;
  const resourceKind = optionalString(url.searchParams.get("resource_kind"), "/resource_kind");
  const resourceName = optionalString(url.searchParams.get("resource_name"), "/resource_name");
  if (resourceKind === null && resourceName === null) return true;
  return (Array.isArray(event.resource_refs) ? event.resource_refs : []).some((ref) =>
    isRecord(ref)
    && (resourceKind === null || ref.kind === resourceKind)
    && (resourceName === null || ref.name === resourceName)
  );
}

async function runtimeResourceLogsResponse(
  cloudRunId: string,
  run: CloudRunRecord,
  runtime: RuntimeRepository,
  route: Extract<RuntimeResourceRoute, { kind: "resource-logs" }>,
  url: URL,
  requester?: string | null,
): Promise<Response> {
  const logs = await runtime.resourceLogs(cloudRunId, run, {
    kind: route.resourceKind,
    name: route.resourceName,
    stream: url.searchParams.get("stream"),
    tailLines: optionalIntFromUrl(url, "tail_lines"),
    requester,
  });
  return runtimeArtifactBytesResponse(logs.object, logs.bytes, {
    ...runtimeResourceByteHeaders(cloudRunId, logs.resource),
    "x-bucephalus-log-stream": logs.stream,
  });
}

async function runtimeResourceContentResponse(
  cloudRunId: string,
  run: CloudRunRecord,
  runtime: RuntimeRepository,
  route: Extract<RuntimeResourceRoute, { kind: "resource-content" }>,
  requester?: string | null,
): Promise<Response> {
  const content = await runtime.resourceArtifactContent(cloudRunId, run, {
    kind: route.resourceKind,
    name: route.resourceName,
    requester,
  });
  return runtimeArtifactBytesResponse(content.object, content.bytes, {
    ...runtimeResourceByteHeaders(cloudRunId, content.resource),
  });
}

function runtimeResourceByteHeaders(cloudRunId: string, resource: { kind: string; metadata: { name: string; resourceVersion?: unknown } }): Record<string, string> {
  const headers: Record<string, string> = {
    "x-bucephalus-run-id": cloudRunId,
    "x-bucephalus-resource-kind": resource.kind,
    "x-bucephalus-resource-name": resource.metadata.name,
  };
  const resourceVersion = typeof resource.metadata.resourceVersion === "string"
    ? resource.metadata.resourceVersion.trim()
    : "";
  if (resourceVersion) {
    headers["x-bucephalus-resource-version"] = resourceVersion;
  }
  return headers;
}

async function runtimeArtifactContentResponse(
  artifact: RuntimeAttemptObjectContentRecord,
  headers: Record<string, string> = {},
): Promise<Response> {
  const bytes = await readStoredObject(artifact.storage_path);
  return runtimeArtifactBytesResponse(artifact, bytes, headers);
}

function runtimeArtifactBytesResponse(
  artifact: Pick<RuntimeAttemptObjectRecord, "core_run_id" | "trial_id" | "role" | "object_ref" | "media_type" | "sha256">,
  bytes: Uint8Array,
  headers: Record<string, string> = {},
): Response {
  return new Response(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer, {
    headers: {
      "content-type": artifact.media_type || "application/octet-stream",
      "content-length": String(bytes.byteLength),
      "x-bucephalus-core-run-id": artifact.core_run_id,
      "x-bucephalus-trial-id": artifact.trial_id,
      "x-bucephalus-artifact-role": artifact.role,
      "x-bucephalus-object-ref": artifact.object_ref,
      "x-bucephalus-sha256": artifact.sha256,
      "x-bucephalus-artifact-sha256": artifact.sha256,
      ...headers,
    },
  });
}

function splitCsv(value: string): string[] {
  return value.split(",").map((item) => item.trim()).filter(Boolean);
}

function runtimeCollectionResourceVersion(values: unknown[]): string {
  return sha256Digest(JSON.stringify(values)).slice("sha256:".length, "sha256:".length + 16);
}

function runtimeTrialResourceName(trialId: string, attempt: number): string {
  const base = runtimeResourceName(trialId);
  return attempt > 0 ? runtimeResourceName(`${base}-attempt-${attempt}`) : base;
}

function runtimeResourceName(value: string): string {
  return value
    .toLowerCase()
    .replace(/^sha256:/, "sha256-")
    .replace(/[^a-z0-9_.-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 96) || "resource";
}

function runtimeResourceUid(...parts: string[]): string {
  return sha256Digest(parts.join("\0"));
}

function runtimeOwnerRef(kind: string, name: string, uid: string): RuntimeResourceRef {
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind,
    name,
    uid,
  };
}

function runtimeRowTimestamp(row: JsonObject): string | undefined {
  for (const candidate of [
    row.recorded_at,
    row.ts,
    row.timestamp,
    row.created_at,
    row.updated_at,
  ]) {
    if (typeof candidate === "string" && candidate.trim()) return candidate.trim();
  }
  return undefined;
}

function jsonObjectOrEmpty(value: unknown): JsonObject {
  return isRecord(value) ? value : {};
}

function stringField(value: unknown): string | null {
  return typeof value === "string" && value.trim().length > 0 ? value.trim() : null;
}

function numberField(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && /^-?\d+(\.\d+)?$/.test(value)) return Number(value);
  return null;
}

function runtimeEventRowFromWire(
  value: unknown,
  pointer: string,
  provenance: { cloudRunId: string; attemptId: string },
): RuntimeEventRowInsert {
  if (!isRecord(value)) {
    throw new HttpError(400, "invalid_row", `${pointer} must be an object`);
  }
  const coreRunId = requireString(value.core_run_id, `${pointer}/core_run_id`);
  if (!CORE_RUN_ID_PATTERN.test(coreRunId)) {
    throw new HttpError(400, "invalid_core_run_id", `${pointer}/core_run_id is not a Core run id`);
  }
  const payload = optionalJsonObject(value.payload as JsonValue | undefined, `${pointer}/payload`);
  if (JSON.stringify(payload).length > INGEST_MAX_ROW_PAYLOAD_BYTES) {
    throw new HttpError(
      400,
      "payload_too_large",
      `${pointer}/payload exceeds ${INGEST_MAX_ROW_PAYLOAD_BYTES} bytes`,
    );
  }
  return {
    core_run_id: coreRunId,
    trial_id: requireString(value.trial_id, `${pointer}/trial_id`),
    schedule_idx: nonNegativeInt(value.schedule_idx, `${pointer}/schedule_idx`),
    attempt: nonNegativeInt(value.attempt, `${pointer}/attempt`),
    row_seq: nonNegativeInt(value.row_seq, `${pointer}/row_seq`),
    slot_commit_id: optionalString(value.slot_commit_id, `${pointer}/slot_commit_id`) ?? "",
    variant_id: optionalString(value.variant_id, `${pointer}/variant_id`) ?? "unknown",
    task_id: optionalString(value.task_id, `${pointer}/task_id`) ?? "unknown",
    repl_idx: nonNegativeInt(value.repl_idx ?? 0, `${pointer}/repl_idx`),
    seq: nonNegativeInt(value.seq ?? 0, `${pointer}/seq`),
    event_type: requireString(value.event_type, `${pointer}/event_type`),
    ts: optionalString(value.ts, `${pointer}/ts`) ?? null,
    payload,
    row: {
      source: "worker_live_ingest",
      cloud_run_id: provenance.cloudRunId,
      attempt_id: provenance.attemptId,
    },
  };
}

function nonNegativeInt(value: unknown, pointer: string): number {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0) {
    throw new HttpError(400, "invalid_integer", `${pointer} must be a non-negative integer`);
  }
  return value;
}

function nonNegativeHeaderInt(request: Request, name: string, scope: string): number {
  const raw = requireHeader(request, name, scope);
  if (!/^[0-9]+$/.test(raw)) {
    throw new HttpError(400, "invalid_header", `${name} must be a non-negative integer`);
  }
  const value = Number.parseInt(raw, 10);
  if (!Number.isSafeInteger(value)) {
    throw new HttpError(400, "invalid_header", `${name} must be a safe integer`);
  }
  return value;
}

function requireCoreRunId(value: string, pointer: string): string {
  if (!CORE_RUN_ID_PATTERN.test(value)) {
    throw new HttpError(400, "invalid_core_run_id", `${pointer} is not a Core run id`);
  }
  return value;
}

function requireArtifactToken(value: string, pointer: string): string {
  if (value.length === 0 || value.length > 256 || value.includes("\0")) {
    throw new HttpError(400, "invalid_request", `${pointer} must be a non-empty artifact identifier`);
  }
  return value;
}

function requireArtifactRole(value: string, pointer: string): string {
  if (!RUNTIME_ARTIFACT_ROLE_PATTERN.test(value)) {
    throw new HttpError(400, "invalid_request", `${pointer} must be a lowercase runtime artifact role`);
  }
  return value;
}

function optionalHeader(request: Request, name: string): string | null {
  const value = request.headers.get(name)?.trim();
  return value && value.length > 0 ? value : null;
}

function runtimeObjectRef(input: {
  coreRunId: string;
  trialId: string;
  attempt: number;
  role: string;
  digest: string;
}): string {
  return `runtime://${[
    input.coreRunId,
    input.trialId,
    String(input.attempt),
    input.role,
    input.digest,
  ].map(encodeURIComponent).join("/")}`;
}

function maxRuntimeArtifactBytes(): number {
  const raw = process.env.BUCEPHALUS_CLOUD_MAX_RUNTIME_ARTIFACT_BYTES;
  if (!raw) {
    return DEFAULT_MAX_RUNTIME_ARTIFACT_BYTES;
  }
  const parsed = Number.parseInt(raw, 10);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : DEFAULT_MAX_RUNTIME_ARTIFACT_BYTES;
}

/**
 * One chronological stream for the run: trial telemetry plus the worker's
 * own lifecycle markers, distinguished by `source` so consumers can filter.
 */
function runtimeEventStreamToWire(
  trialRows: RuntimeEventRecord[],
  workerEvents: WorkerLifecycleEventRecord[],
  afterRowSeq: number | null,
): JsonObject[] {
  const trialWire = trialRows.map((row) => ({
    ...row,
    source: "trial",
    event_id: `trial:${row.core_run_id}:${row.trial_id}:${row.schedule_idx}:${row.attempt}:${row.row_seq}`,
    recorded_at: row.ts,
    resource_refs: [runtimeOwnerRef(
      "Trial",
      runtimeTrialResourceName(row.trial_id, row.attempt),
      runtimeResourceUid("Trial", "", row.core_run_id, row.trial_id, String(row.attempt)),
    )],
  }));
  const workerWire = workerEvents.map((event) => ({
    source: "worker",
    event_id: `worker:${event.event_id}`,
    core_run_id: "",
    trial_id: "",
    schedule_idx: 0,
    attempt: 0,
    row_seq: event.seq,
    slot_commit_id: "",
    variant_id: "worker",
    task_id: "",
    repl_idx: 0,
    seq: event.seq,
    event_type: event.event_type,
    ts: event.created_at,
    recorded_at: event.created_at,
    resource_refs: [],
    payload: event.payload,
    row: { source: "worker_lifecycle" },
  }));
  // Rows without a timestamp sort to the end rather than masquerading as the
  // oldest events in the stream.
  const sortKey = (row: { recorded_at: string | null }) => row.recorded_at ?? "￿";
  return ([...workerWire, ...trialWire]
    .filter((row) => row.row_seq > (afterRowSeq ?? -1))
    .sort((a, b) => sortKey(a).localeCompare(sortKey(b))) as JsonObject[]);
}

async function completeAttempt(
  request: Request,
  url: URL,
  runs: RunRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const attemptId = attemptIdFromWorkerPath(url.pathname, "/complete");
  const runnerInstanceId = requireString(body.runner_instance_id, "/runner_instance_id");
  await requireAttemptToken(request, runs, { attemptId, runnerInstanceId });
  const result = await runs.completeAttempt({
    attemptId,
    runnerInstanceId,
  });
  return jsonResponse(claimToWire(result));
}

async function failAttempt(
  request: Request,
  url: URL,
  runs: RunRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const attemptId = attemptIdFromWorkerPath(url.pathname, "/fail");
  const runnerInstanceId = requireString(body.runner_instance_id, "/runner_instance_id");
  await requireAttemptToken(request, runs, { attemptId, runnerInstanceId });
  const message = requireString(body.message, "/message");
  const result = await runs.failAttempt({
    attemptId,
    runnerInstanceId,
    message,
  });
  // The durable record lives in Postgres, but operators debug from Cloud
  // Logging; an attempt failure must be visible there without a DB query.
  logError("run.attempt_failed", newTraceContext({ component: "api", runId: result.run.run_id, attemptId }), {
    run_id: result.run.run_id,
    attempt_id: attemptId,
    runner_instance_id: runnerInstanceId,
    error: message,
  });
  return jsonResponse(claimToWire(result));
}

async function workerListPortForwards(
  request: Request,
  route: { attemptId: string },
  runs: RunRepository,
  runtime: RuntimeRepository,
): Promise<Response> {
  const runnerInstanceId = requireString(new URL(request.url).searchParams.get("runner_instance_id"), "/runner_instance_id");
  await requireAttemptToken(request, runs, {
    attemptId: route.attemptId,
    runnerInstanceId,
  });
  const requests = await runtime.portForwardRequestsForAttempt({
    attemptId: route.attemptId,
    runnerInstanceId,
  });
  return jsonResponse({
    resources: requests.map((item) => runtime.runtimeAccessRequestResourceForRunId(item.run_id, item)),
  });
}

async function workerUpdatePortForward(
  request: Request,
  route: { attemptId: string; accessRequestId: string; action: string },
  runs: RunRepository,
  runtime: RuntimeRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const runnerInstanceId = requireString(body.runner_instance_id, "/runner_instance_id");
  await requireAttemptToken(request, runs, {
    attemptId: route.attemptId,
    runnerInstanceId,
  });
  const portForward = await runtime.updatePortForwardRequest({
    attemptId: route.attemptId,
    runnerInstanceId,
    accessRequestId: route.accessRequestId,
    status: portForwardStatusForWorkerAction(route.action),
    connection: body.connection === undefined ? null : optionalJsonObject(body.connection as JsonValue | undefined, "/connection"),
    errorMessage: optionalString(body.error_message, "/error_message"),
  });
  return jsonResponse({ resource: runtime.runtimeAccessRequestResourceForRunId(portForward.run_id, portForward) });
}

async function workerListExecRequests(
  request: Request,
  route: { attemptId: string },
  runs: RunRepository,
  runtime: RuntimeRepository,
): Promise<Response> {
  const runnerInstanceId = requireString(new URL(request.url).searchParams.get("runner_instance_id"), "/runner_instance_id");
  await requireAttemptToken(request, runs, {
    attemptId: route.attemptId,
    runnerInstanceId,
  });
  const requests = await runtime.execRequestsForAttempt({
    attemptId: route.attemptId,
    runnerInstanceId,
  });
  return jsonResponse({
    resources: requests.map((item) => runtime.runtimeAccessRequestResourceForRunId(item.run_id, item)),
  });
}

async function workerUpdateExecRequest(
  request: Request,
  route: { attemptId: string; accessRequestId: string; action: string },
  runs: RunRepository,
  runtime: RuntimeRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const runnerInstanceId = requireString(body.runner_instance_id, "/runner_instance_id");
  await requireAttemptToken(request, runs, {
    attemptId: route.attemptId,
    runnerInstanceId,
  });
  const exec = await runtime.updateExecRequest({
    attemptId: route.attemptId,
    runnerInstanceId,
    accessRequestId: route.accessRequestId,
    status: execStatusForWorkerAction(route.action),
    connection: body.connection === undefined ? null : optionalJsonObject(body.connection as JsonValue | undefined, "/connection"),
    errorMessage: optionalString(body.error_message, "/error_message"),
  });
  return jsonResponse({ resource: runtime.runtimeAccessRequestResourceForRunId(exec.run_id, exec) });
}

async function expireLeases(runs: RunRepository, runtime: RuntimeRepository): Promise<Response> {
  const [expired, expiredAccessRequests] = await Promise.all([
    runs.expireLeases(),
    runtime.expireRuntimeAccessRequests(),
  ]);
  return jsonResponse({
    expired: expired.map((item) => claimToWire(item)),
    expired_access_resources: expiredAccessRequests.map((item) => runtime.runtimeAccessRequestResourceForRunId(item.run_id, item)),
  });
}

async function createRun(
  request: Request,
  packages: PackageRepository,
  runs: RunRepository,
  runners: RunnerRepository,
  ownerKey?: string,
  secrets?: CloudSecretRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const env = cloudRunEnv(requireStringMap(body.env, "/env"));
  const diagnosis = await diagnoseCloudRunRequest({
    body,
    packages,
    runners,
    ownerKey,
    secrets,
    requireHostedSecretRefs: true,
  });
  rejectEnvSecretCollisions(env, diagnosis.secretRefs);

  const run = await runs.createRun({
    packageDigest: diagnosis.artifact.package_digest,
    runLabel: optionalString(body.run_label, "/run_label"),
    env,
    secretRefs: await resolveHostedSecretRefs(diagnosis.secretRefs, secrets, ownerKey),
    runtimeOptions: diagnosis.runtimeOptions,
    ownerKey,
    runRequirements: diagnosis.runRequirements,
    packageProvenance: diagnosis.artifact.package_provenance,
  });
  return jsonResponse(runToWire(run), { status: 201 });
}

const CLOUD_RUN_ENV_NAME_PATTERN = /^[A-Z_][A-Z0-9_]*$/;
const CLOUD_RUN_ENV_VALUE_MAX_BYTES = 32 * 1024;
const textEncoder = new TextEncoder();

function cloudRunEnv(env: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [name, value] of Object.entries(env)) {
    if (!CLOUD_RUN_ENV_NAME_PATTERN.test(name)) {
      throw new HttpError(
        400,
        "invalid_run_env",
        `/env contains invalid environment variable name '${name}'. Use uppercase shell identifiers matching ${CLOUD_RUN_ENV_NAME_PATTERN.source}`,
      );
    }
    const reserved = controlPlaneEnvNameViolation(name);
    if (reserved) {
      throw new HttpError(400, "reserved_run_env", reserved);
    }
    if (value.includes("\0")) {
      throw new HttpError(400, "invalid_run_env", `/env/${name} must not contain NUL bytes`);
    }
    if (textEncoder.encode(value).byteLength > CLOUD_RUN_ENV_VALUE_MAX_BYTES) {
      throw new HttpError(
        400,
        "invalid_run_env",
        `/env/${name} exceeds ${CLOUD_RUN_ENV_VALUE_MAX_BYTES} bytes`,
      );
    }
    out[name] = value;
  }
  return out;
}

function rejectEnvSecretCollisions(env: Record<string, string>, secretRefs: Record<string, string>): void {
  for (const name of Object.keys(env)) {
    if (Object.prototype.hasOwnProperty.call(secretRefs, name)) {
      throw new HttpError(
        400,
        "run_env_secret_collision",
        `/env/${name} cannot also be supplied in /secret_refs. Use --env for plain config or --secret-ref for secrets, not both.`,
      );
    }
  }
}

export interface CloudRunDiagnosis {
  artifact: PackageArtifactRecord;
  secretRefs: Record<string, string>;
  runtimeOptions: JsonObject;
  runRequirements: RunRequirements;
}

export async function diagnoseCloudRunRequest(input: {
  body: Record<string, unknown>;
  packages: PackageRepository;
  runners: RunnerRepository;
  ownerKey?: string | undefined;
  secrets?: CloudSecretRepository | undefined;
  requireHostedSecretRefs?: boolean;
}): Promise<CloudRunDiagnosis> {
  const packageDigest = requirePackageDigest(input.body.package_digest, "/package_digest");
  const artifact = await input.packages.getArtifact(packageDigest, input.ownerKey);
  if (!artifact) {
    throw new HttpError(404, "package_not_found", "Package artifact not found");
  }
  if (artifact.status !== "accepted") {
    throw new HttpError(409, "package_not_runnable", "Package artifact is not accepted");
  }

  const secretRefs = cloudSecretRefs(requireStringMap(input.body.secret_refs, "/secret_refs"));
  if (input.requireHostedSecretRefs) {
    await resolveHostedSecretRefs(secretRefs, input.secrets, input.ownerKey);
  }
  const runtimeOptions = optionalJsonObject(input.body.runtime_options as JsonValue | undefined, "/runtime_options");
  validatePackageSecretRefs(artifact, secretRefs);
  const runRequirements = runRequirementsForArtifact(
    artifact,
    runtimeOptions,
    secretRefs,
  );
  await requireSchedulableRun(input.runners, runRequirements);
  return {
    artifact,
    secretRefs,
    runtimeOptions,
    runRequirements,
  };
}

export async function requireSchedulableRun(
  runners: RunnerRepository,
  requirements: RunRequirements,
): Promise<void> {
  const pools = await runners.listPools();
  const activePools = pools.filter((pool) => pool.status === "active");
  const runnablePools = activePools.filter((pool) => pool.active_worker_image_id);
  if (runnablePools.some((pool) => poolSatisfiesRun(pool, requirements))) {
    return;
  }
  const matchingPoolsWithoutWorker = activePools
    .filter((pool) => !pool.active_worker_image_id && poolSatisfiesRun(pool, requirements));
  if (matchingPoolsWithoutWorker.length > 0) {
    throw new HttpError(
      409,
      "runner_pool_worker_image_missing",
      "An active runner pool can satisfy this run, but it has no active worker image promoted",
      {
        required_executor: requirements.executor,
        required_resources: requirements.requires,
        matching_pools_without_worker_image: matchingPoolsWithoutWorker.map((pool) => ({
          runner_pool_id: pool.runner_pool_id,
          name: pool.name,
          capabilities: pool.capabilities as unknown as JsonObject,
        })),
      },
    );
  }
  throw new HttpError(
    409,
    "run_unschedulable",
    `No active runner pool can satisfy this run. Required executor '${requirements.executor}' with resources: ${requirements.requires.join(", ") || "<none>"}`,
    {
      required_executor: requirements.executor,
      required_resources: requirements.requires,
      active_pools: activePools.map((pool) => ({
        runner_pool_id: pool.runner_pool_id,
        name: pool.name,
        active_worker_image_id: pool.active_worker_image_id,
        capabilities: pool.capabilities as unknown as JsonObject,
      })),
    },
  );
}

async function pendingReasonsForRuns(
  runs: CloudRunRecord[],
  runners: RunnerRepository,
): Promise<Map<string, PendingReason>> {
  if (runs.length === 0) {
    return new Map();
  }
  const [pools, instances] = await Promise.all([
    runners.listPools(),
    runners.listInstances({ limit: 500 }),
  ]);
  return pendingReasonsFromRunnerState(runs, pools, instances);
}

function pendingReasonsFromRunnerState(
  runs: CloudRunRecord[],
  pools: RunnerPoolRecord[],
  instances: RunnerInstanceRecord[],
): Map<string, PendingReason> {
  const activePools = pools.filter((pool) => pool.status === "active");
  const runnablePools = activePools.filter((pool) => pool.active_worker_image_id);
  const onlineInstances = instances.filter((instance) => instance.status === "online");
  return new Map(runs.map((run) => {
    const matchingPools = runnablePools.filter((pool) => poolSatisfiesRun(pool, run.run_requirements));
    const matchingInstances = onlineInstances.filter((instance) =>
      matchingPools.some((pool) => pool.runner_pool_id === instance.runner_pool_id)
      && capabilitiesSatisfyRun(instance.capabilities, run.run_requirements)
    );
    return [
      run.run_id,
      matchingPools.length > 0 || matchingInstances.length > 0
        ? "waiting_for_capacity"
        : "no_matching_runner",
    ];
  }));
}

function isQueuedRunStatus(status: string): boolean {
  return status === "created" || status === "waiting_for_runner";
}

function poolSatisfiesRun(pool: RunnerPoolRecord, requirements: RunRequirements): boolean {
  return capabilitiesSatisfyRun(pool.capabilities, requirements);
}

function capabilitiesSatisfyRun(capabilities: WorkerCapabilities, requirements: RunRequirements): boolean {
  const isolation = capabilities.isolation ?? [];
  return capabilities.executors.includes(requirements.executor)
    && requirements.requires.every((resource) => capabilities.resources.includes(resource))
    && (!requirements.arch || !capabilities.arch || capabilities.arch === requirements.arch)
    && (!requirements.cpu_count || !capabilities.cpu_count || capabilities.cpu_count >= requirements.cpu_count)
    && (!requirements.memory_mb || !capabilities.memory_mb || capabilities.memory_mb >= requirements.memory_mb)
    && (!requirements.disk_mb || !capabilities.disk_mb || capabilities.disk_mb >= requirements.disk_mb)
    && (!requirements.isolation || isolation.length === 0 || isolation.includes(requirements.isolation));
}

export function runRequirementsForArtifact(
  artifact: PackageArtifactRecord,
  runtimeOptions: JsonObject,
  secretRefs: Record<string, string> = {},
): RunRequirements {
  validateCloudRuntimeOptions(runtimeOptions);
  const requestedBackend = optionalString(runtimeOptions.backend, "/runtime_options/backend")
    ?? packageComputeBackend(artifact);
  const executor = cloudExecutorForBackend(requestedBackend);
  rejectUnsupportedCloudTrialRuntime(artifact, runtimeOptions);
  const imageRefs = artifact.image_refs ?? [];
  const invalidImages = process.env.BUCEPHALUS_CLOUD_ALLOW_LOCAL_IMAGE_REFS === "true"
    ? []
    : imageRefs.filter((ref) => !isCloudDigestPinnedImageRef(ref));
  if (invalidImages.length > 0) {
    throw new HttpError(
      409,
      "package_images_not_cloud_pinned",
      `Package references image(s) that are not digest-pinned remote registry refs for Cloud runs: ${invalidImages.join(", ")}`,
    );
  }
  const requires = executor === "runner-docker"
    ? ["core_runner", "docker_daemon", "registry_pull"]
    : ["core_runner", "modal", "registry_pull"];
  const secretIds = Object.keys(cloudSecretRefs(secretRefs)).sort();
  if (secretIds.length > 0) {
    requires.push("secret_resolver");
  }
  const networkPerimeter = cloudNetworkPerimeter(artifact, runtimeOptions);
  if (networkPerimeter.egress_hosts.length > 0) {
    requires.push("network_perimeter");
  }
  const sidecars = cloudStringList(runtimeOptions.sidecars, "/runtime_options/sidecars");
  const accelerators = cloudStringList(runtimeOptions.accelerators, "/runtime_options/accelerators");
  const requestedIsolation = cloudIsolation(runtimeOptions.isolation) ?? cloudIsolation(packageRuntimeValue(artifact, "isolation"));
  const requestedArch = cloudArch(runtimeOptions.arch);
  const packageArch = cloudArch(packageRuntimeValue(artifact, "arch"));
  const targetArch = cloudArchFromPackageTarget(artifact);
  const resolvedArch = requestedArch ?? packageArch ?? targetArch ?? "x86_64";
  if (targetArch && requestedArch && targetArch !== requestedArch) {
    throw new HttpError(
      400,
      "cloud_arch_mismatch",
      `Requested Cloud runner architecture '${requestedArch}' does not match package task image platform architecture '${targetArch}'`,
    );
  }
  if (targetArch && packageArch && targetArch !== packageArch) {
    throw new HttpError(
      409,
      "package_arch_mismatch",
      `Package runtime architecture '${packageArch}' does not match task image platform architecture '${targetArch}'`,
    );
  }
  if (networkPerimeter.egress_hosts.length > 0 && requestedIsolation === "reusable_vm") {
    throw new HttpError(
      400,
      "unsupported_cloud_network_isolation",
      "Cloud runs with runtime network egress allowlists require isolation=single_use_vm",
    );
  }
  for (const sidecar of sidecars) {
    requires.push(`sidecar:${sidecar}`);
  }
  for (const accelerator of accelerators) {
    requires.push(`accelerator:${accelerator}`);
  }
  return {
    executor,
    requires: [...new Set(requires)],
    image_refs: imageRefs,
    secret_ids: secretIds,
    network_perimeter: networkPerimeter,
    sidecars,
    accelerators,
    arch: resolvedArch,
    cpu_count: positiveInt(runtimeOptions.cpu_count) ?? positiveInt(packageRuntimeValue(artifact, "cpu_count")) ?? 1,
    memory_mb: positiveInt(runtimeOptions.memory_mb) ?? positiveInt(packageRuntimeValue(artifact, "memory_mb")) ?? 1024,
    disk_mb: positiveInt(runtimeOptions.disk_mb) ?? positiveInt(packageRuntimeValue(artifact, "disk_mb")) ?? 20480,
    isolation: networkPerimeter.egress_hosts.length > 0
      ? "single_use_vm"
      : requestedIsolation ?? "reusable_vm",
    timeout_ms: positiveInt(runtimeOptions.timeout_ms) ?? positiveInt(packageRuntimeValue(artifact, "timeout_ms")),
    max_parallel_trials: positiveInt(runtimeOptions.max_parallel_trials)
      ?? positiveInt(packageRuntimeValue(artifact, "max_parallel_trials"))
      ?? 1,
  };
}

const CLOUD_RUNTIME_OPTION_KEYS = new Set([
  "backend",
  "arch",
  "cpu_count",
  "memory_mb",
  "disk_mb",
  "isolation",
  "timeout_ms",
  "max_parallel_trials",
  "network",
  "sidecars",
  "accelerators",
]);

const CLOUD_RUNTIME_NETWORK_KEYS = new Set([
  "default",
  "task_sandbox",
  "agent",
  "egress",
]);

function validateCloudRuntimeOptions(runtimeOptions: JsonObject): void {
  for (const [key, value] of Object.entries(runtimeOptions)) {
    if (!CLOUD_RUNTIME_OPTION_KEYS.has(key)) {
      throw new HttpError(
        400,
        "unknown_cloud_runtime_option",
        `/runtime_options/${escapeJsonPointer(key)} is not supported for hosted Cloud runs`,
        {
          option: key,
          supported_options: [...CLOUD_RUNTIME_OPTION_KEYS].sort(),
        },
      );
    }
    validateCloudRuntimeOptionValue(key, value);
  }
}

function validateCloudRuntimeOptionValue(key: string, value: unknown): void {
  const pointer = `/runtime_options/${escapeJsonPointer(key)}`;
  if (["backend", "arch", "isolation"].includes(key)) {
    if (typeof value !== "string" || value.trim().length === 0) {
      throw new HttpError(400, "invalid_cloud_runtime_option", `${pointer} must be a non-empty string`);
    }
    return;
  }
  if (["cpu_count", "memory_mb", "disk_mb", "timeout_ms", "max_parallel_trials"].includes(key)) {
    if (positiveInt(value) === null) {
      throw new HttpError(400, "invalid_cloud_runtime_option", `${pointer} must be a positive integer`);
    }
    return;
  }
  if (key === "sidecars" || key === "accelerators") {
    cloudStringList(value, pointer);
    return;
  }
  if (key === "network") {
    validateCloudRuntimeNetworkOption(value, pointer);
  }
}

function validateCloudRuntimeNetworkOption(value: unknown, pointer: string): void {
  if (!isRecord(value)) {
    throw new HttpError(400, "invalid_cloud_runtime_option", `${pointer} must be an object`);
  }
  for (const [key, item] of Object.entries(value)) {
    if (!CLOUD_RUNTIME_NETWORK_KEYS.has(key)) {
      throw new HttpError(
        400,
        "unknown_cloud_runtime_option",
        `${pointer}/${escapeJsonPointer(key)} is not supported for hosted Cloud network options`,
        {
          option: `network.${key}`,
          supported_options: [...CLOUD_RUNTIME_NETWORK_KEYS].map((item) => `network.${item}`).sort(),
        },
      );
    }
    if (key === "egress") {
      cloudEgressHosts(item);
    } else if (typeof item !== "string" || item.trim().length === 0) {
      throw new HttpError(400, "invalid_cloud_runtime_option", `${pointer}/${escapeJsonPointer(key)} must be a non-empty string`);
    }
  }
}

function cloudNetworkPerimeter(
  artifact: PackageArtifactRecord,
  runtimeOptions: JsonObject,
): RunRequirements["network_perimeter"] {
  const packageNetwork = networkObject(packageRuntimeTopLevelValue(artifact, "network"));
  const overrideNetwork = networkObject(runtimeOptions.network);
  const runtimeNetwork = mergedNetworkObject(packageNetwork, overrideNetwork);
  if (!runtimeNetwork) {
    return {
      default: "none",
      task_sandbox: "none",
      agent: "none",
      egress_hosts: [],
    };
  }

  const defaultMode = cloudNetworkMode(runtimeNetwork.default, "/runtime/network/default");
  const taskSandboxMode = cloudNetworkMode(runtimeNetwork.task_sandbox, "/runtime/network/task_sandbox");
  const agentMode = cloudNetworkMode(runtimeNetwork.agent, "/runtime/network/agent");
  const egressHosts = cloudEgressHosts(runtimeNetwork.egress);
  const hasAllowlistMode = [defaultMode, taskSandboxMode, agentMode].includes("allowlist_enforced");
  if (hasAllowlistMode && egressHosts.length === 0) {
    throw new HttpError(
      400,
      "unsupported_cloud_network_egress",
      "runtime.network.egress must declare at least one hostname when a Cloud network mode is allowlist_enforced",
    );
  }

  return {
    default: defaultMode ?? "none",
    task_sandbox: taskSandboxMode ?? defaultMode ?? "none",
    agent: agentMode ?? defaultMode ?? "none",
    egress_hosts: egressHosts,
  };
}

function mergedNetworkObject(
  packageNetwork: Record<string, unknown> | null,
  overrideNetwork: Record<string, unknown> | null,
): Record<string, unknown> | null {
  if (!packageNetwork) {
    return overrideNetwork;
  }
  if (!overrideNetwork) {
    return packageNetwork;
  }
  return {
    ...packageNetwork,
    ...overrideNetwork,
  };
}

function cloudNetworkMode(value: unknown, pointer: string): RunNetworkMode | null {
  const mode = optionalString(value, pointer);
  if (!mode) {
    return null;
  }
  if (mode === "none" || mode === "allowlist_enforced") {
    return mode;
  }
  throw new HttpError(
    400,
    "unsupported_cloud_network_mode",
    `${pointer}='${mode}' is not supported for Cloud runs; use 'none' or 'allowlist_enforced' with explicit runtime.network.egress hosts`,
  );
}

function networkObject(value: unknown): Record<string, unknown> | null {
  return isRecord(value) ? value : null;
}

function cloudEgressHosts(value: unknown): string[] {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new HttpError(400, "unsupported_cloud_network_egress", "runtime.network.egress must be an array of hostnames");
  }
  const hosts = value.map((item) => {
    if (typeof item !== "string" || item.trim().length === 0) {
      throw new HttpError(400, "unsupported_cloud_network_egress", "runtime.network.egress entries must be non-empty hostnames");
    }
    return cloudEgressHost(item);
  });
  return [...new Set(hosts)].sort();
}

function cloudEgressHost(value: string): string {
  const host = value.trim().toLowerCase();
  if (
    host.includes("://")
    || host.includes("/")
    || host.includes("*")
    || host === "localhost"
    || host.startsWith("localhost:")
    || host.startsWith("127.")
    || host === "0.0.0.0"
  ) {
    throw new HttpError(400, "unsupported_cloud_network_egress", `Unsupported Cloud egress host '${value}'`);
  }
  if (!/^[a-z0-9.-]+(:[0-9]+)?$/.test(host)) {
    throw new HttpError(400, "unsupported_cloud_network_egress", `Unsupported Cloud egress host '${value}'`);
  }
  return host;
}

function cloudSecretRefs(secretRefs: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  const allowControlPlaneRefs = allowsControlPlaneSecretRefs();
  for (const [id, ref] of Object.entries(secretRefs)) {
    if (!/^[A-Za-z0-9_.-]+$/.test(id)) {
      throw new HttpError(400, "unsupported_cloud_secret_ref", `Invalid Cloud secret id '${id}'`);
    }
    if (ref.trim().length === 0 || ref.includes("\n") || ref.includes("\r")) {
      throw new HttpError(400, "unsupported_cloud_secret_ref", `Invalid Cloud secret ref for '${id}'`);
    }
    if (!supportedCloudSecretRef(ref)) {
      throw new HttpError(
        400,
        "unsupported_cloud_secret_ref",
        `Unsupported Cloud secret ref for '${id}'. Use bucephalus://<name> for hosted secrets, gcp-secret-manager://..., or aws-secrets-manager://...`,
      );
    }
    if (!allowControlPlaneRefs) {
      const idViolation = controlPlaneSecretIdViolation(id);
      if (idViolation) {
        throw new HttpError(400, "reserved_cloud_secret_ref", idViolation);
      }
      const refViolation = controlPlaneSecretRefViolation(ref);
      if (refViolation) {
        throw new HttpError(400, "reserved_cloud_secret_ref", refViolation);
      }
    }
    out[id] = ref.trim();
  }
  return out;
}

function supportedCloudSecretRef(value: string): boolean {
  const ref = value.trim();
  return ref.startsWith(HOSTED_SECRET_REF_PREFIX)
    || ref.startsWith("gcp-secret-manager://")
    || ref.startsWith("aws-secrets-manager://");
}

// Hosted refs are resolved to backing provider refs at run creation, so the
// stored run (and the worker claim payload built from it) only ever carries
// refs the attempt-scoped secret resolver understands.
async function resolveHostedSecretRefs(
  secretRefs: Record<string, string>,
  secrets: CloudSecretRepository | undefined,
  ownerKey: string | undefined,
): Promise<Record<string, string>> {
  const out: Record<string, string> = {};
  for (const [id, ref] of Object.entries(secretRefs)) {
    if (!ref.startsWith(HOSTED_SECRET_REF_PREFIX)) {
      out[id] = ref;
      continue;
    }
    const name = ref.slice(HOSTED_SECRET_REF_PREFIX.length);
    if (!SECRET_NAME_PATTERN.test(name)) {
      throw new HttpError(400, "unsupported_cloud_secret_ref", `Invalid hosted secret name in ref '${ref}'`);
    }
    if (!secrets || !ownerKey) {
      throw new HttpError(400, "hosted_secrets_unavailable", "Hosted secret refs require an authenticated owner");
    }
    const record = await secrets.getSecret(ownerKey, name);
    if (!record) {
      throw new HttpError(400, "unknown_hosted_secret", `No hosted secret named '${name}'. Upload it with \`buc secrets put ${name} --from-env ${name}\` first.`, {
        secret_id: id,
        secret_name: name,
        next: `buc secrets put ${name} --from-env ${name}`,
      });
    }
    out[id] = record.backing_ref;
  }
  return out;
}

function rejectUnsupportedCloudTrialRuntime(
  artifact: PackageArtifactRecord,
  runtimeOptions: JsonObject,
): void {
  const agentSite = trialRuntimeAgentSite(runtimeOptions)
    ?? trialRuntimeAgentSite(artifact.resolved_experiment_json);
  if (agentSite === "host") {
    throw new HttpError(
      400,
      "unsupported_cloud_agent_site",
      "Cloud runs do not support trial_runtime.execution.agent_site=host",
    );
  }
}

function trialRuntimeAgentSite(value: JsonObject): string | null {
  const trialRuntime = value.trial_runtime;
  if (!isRecord(trialRuntime)) {
    return null;
  }
  const execution = trialRuntime.execution;
  if (!isRecord(execution) || typeof execution.agent_site !== "string") {
    return null;
  }
  return execution.agent_site.trim();
}

function cloudStringList(value: unknown, pointer: string): string[] {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new HttpError(400, "unsupported_cloud_runtime_requirement", `${pointer} must be an array of strings`);
  }
  return [...new Set(value.map((item) => {
    if (typeof item !== "string" || item.trim().length === 0) {
      throw new HttpError(400, "unsupported_cloud_runtime_requirement", `${pointer} entries must be non-empty strings`);
    }
    return item.trim();
  }))].sort();
}

function packageComputeBackend(artifact: PackageArtifactRecord): string {
  const backend = artifact.resolved_experiment_json
    .runtime;
  if (isRecord(backend)) {
    const compute = backend.compute;
    if (isRecord(compute) && typeof compute.backend === "string") {
      return compute.backend;
    }
  }
  return "runner-docker";
}

function packageRuntimeValue(artifact: PackageArtifactRecord, key: string): unknown {
  const compute = packageRuntimeTopLevelValue(artifact, "compute");
  if (!isRecord(compute)) {
    return undefined;
  }
  return compute[key];
}

function packageRuntimeTopLevelValue(artifact: PackageArtifactRecord, key: string): unknown {
  const runtime = artifact.resolved_experiment_json.runtime;
  if (!isRecord(runtime)) {
    return undefined;
  }
  return runtime[key];
}

function cloudExecutorForBackend(backend: string): RunRequirements["executor"] {
  switch (backend) {
    case "runner-docker":
    case "runner_docker":
      return "runner-docker";
    case "local-docker":
    case "local_docker":
      return "runner-docker";
    case "modal":
      return "modal";
    default:
      throw new HttpError(400, "unsupported_cloud_backend", `Unsupported Cloud run backend '${backend}'`);
  }
}

function isCloudDigestPinnedImageRef(ref: string): boolean {
  const trimmed = ref.trim();
  if (trimmed.length === 0) {
    return false;
  }
  const firstComponent = trimmed.split("/")[0] ?? "";
  if (firstComponent === "localhost" || firstComponent.startsWith("localhost:") || firstComponent.startsWith("127.")) {
    return false;
  }
  return trimmed.includes("/")
    && (firstComponent.includes(".") || firstComponent.includes(":"))
    && /@sha256:[0-9a-f]{64}$/.test(trimmed);
}

function cloudArch(value: unknown): RunRequirements["arch"] | null {
  if (typeof value !== "string") {
    return null;
  }
  switch (value.trim().toLowerCase()) {
    case "x86_64":
    case "amd64":
      return "x86_64";
    case "arm64":
    case "aarch64":
      return "arm64";
    default:
      throw new HttpError(400, "unsupported_cloud_arch", `Unsupported Cloud runner architecture '${value}'`);
  }
}

function cloudArchFromPackageTarget(artifact: PackageArtifactRecord): RunRequirements["arch"] | null {
  const target = artifact.target;
  if (!isRecord(target)) {
    return null;
  }
  const platforms = Array.isArray(target.task_platforms)
    ? target.task_platforms.filter((platform): platform is string => typeof platform === "string")
    : [];
  const arches = [...new Set(platforms.map(cloudArchFromPlatform).filter((arch): arch is RunRequirements["arch"] => arch !== null))];
  if (arches.length === 0) {
    return null;
  }
  if (arches.length > 1) {
    throw new HttpError(
      409,
      "mixed_task_image_platforms",
      `Package declares task image platforms that require multiple Cloud runner architectures: ${platforms.join(", ")}`,
    );
  }
  return arches[0] ?? null;
}

function cloudArchFromPlatform(platform: string): RunRequirements["arch"] | null {
  const value = platform.trim().toLowerCase();
  switch (value) {
    case "linux/amd64":
    case "linux/x86_64":
    case "amd64":
    case "x86_64":
      return "x86_64";
    case "linux/arm64":
    case "linux/aarch64":
    case "arm64":
    case "aarch64":
      return "arm64";
    default:
      throw new HttpError(
        409,
        "unsupported_task_image_platform",
        `Unsupported Cloud task image platform '${platform}'`,
      );
  }
}

function cloudIsolation(value: unknown): RunRequirements["isolation"] | null {
  if (typeof value !== "string") {
    return null;
  }
  switch (value.trim()) {
    case "reusable_vm":
    case "single_use_vm":
      return value.trim() as RunRequirements["isolation"];
    default:
      throw new HttpError(400, "unsupported_cloud_isolation", `Unsupported Cloud isolation mode '${value}'`);
  }
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

function isRecord(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function escapeJsonPointer(value: string): string {
  return value.replaceAll("~", "~0").replaceAll("/", "~1");
}

function packageToWire(artifact: PackageArtifactRecord, options: { includeConfig: boolean }) {
  const summary = {
    package_digest: artifact.package_digest,
    name: packageName(artifact),
    description: packageDescription(artifact),
    tags: packageTags(artifact),
    owner: packageOwner(artifact),
    upload_id: artifact.upload_id,
    byte_size: nullableNumber(artifact.byte_size),
    media_type: artifact.media_type,
    target: artifact.target,
    secret_requirements: packageSecretRequirements(artifact),
    status: artifact.status,
    created_at: artifact.created_at,
    updated_at: artifact.updated_at,
  };
  if (!options.includeConfig) {
    return summary;
  }
  return {
    ...summary,
    manifest_json: artifact.manifest_json,
    resolved_experiment_json: artifact.resolved_experiment_json,
    image_refs: artifact.image_refs,
    package_provenance: artifact.package_provenance,
    diagnostics: artifact.diagnostics,
  };
}

async function enrichRuns(
  runs: CloudRunRecord[],
  packages: PackageRepository,
  runtime: RuntimeRepository,
  runners: RunnerRepository,
  ownerKey?: string,
  options: { includeRuntime?: boolean } = {},
): Promise<Map<string, RunWireEnrichment>> {
  if (runs.length === 0) {
    return new Map();
  }
  const runIds = runs.map((run) => run.run_id);
  const packageDigests = [...new Set(runs.map((run) => run.package_digest))].sort();
  const includeRuntime = options.includeRuntime ?? true;
  const queuedRuns = includeRuntime ? runs.filter((run) => isQueuedRunStatus(run.status)) : [];
  const [artifacts, progress, pendingReasons] = await Promise.all([
    packages.listArtifactsByDigests(packageDigests, ownerKey),
    includeRuntime ? runtime.trialProgressForCloudRuns(runIds) : Promise.resolve([]),
    includeRuntime ? pendingReasonsForRuns(queuedRuns, runners) : Promise.resolve(new Map<string, PendingReason>()),
  ]);
  const artifactByDigest = new Map(artifacts.map((artifact) => [artifact.package_digest, artifact]));
  const progressByRunId = new Map(progress.map((item) => [item.cloud_run_id, item]));
  return new Map(runs.map((run) => {
    const artifact = artifactByDigest.get(run.package_digest) ?? null;
    const trialProgress = progressByRunId.get(run.run_id);
    return [run.run_id, {
      experiment_name: artifact ? packageName(artifact) : null,
      trials_completed: trialProgress?.trials_completed ?? null,
      trials_total: trialProgress?.trials_total ?? null,
      pending_reason: pendingReasons.get(run.run_id) ?? null,
    }];
  }));
}

// Packages need a human-readable identity wherever they are listed; without
// one, consumers fall back to stringifying structures. The authoritative name
// is the resolved experiment's name sealed inside the package.
function packageName(artifact: PackageArtifactRecord): string | null {
  for (const candidate of [
    jsonPointerValue(artifact.resolved_experiment_json, "/experiment/name"),
    jsonPointerValue(artifact.manifest_json, "/resolved_experiment/experiment/name"),
  ]) {
    if (typeof candidate === "string" && candidate.trim().length > 0) {
      return candidate.trim();
    }
  }
  return null;
}

function packageDescription(artifact: PackageArtifactRecord): string | null {
  for (const candidate of [
    jsonPointerValue(artifact.manifest_json, "/description"),
    jsonPointerValue(artifact.resolved_experiment_json, "/description"),
    jsonPointerValue(artifact.resolved_experiment_json, "/experiment/description"),
  ]) {
    if (typeof candidate === "string" && candidate.trim()) {
      return candidate.trim();
    }
  }
  return null;
}

function packageTags(artifact: PackageArtifactRecord): string[] {
  const manifestTags = jsonPointerValue(artifact.manifest_json, "/tags");
  const resolvedTags = jsonPointerValue(artifact.resolved_experiment_json, "/tags");
  return [...new Set([...stringArray(manifestTags), ...stringArray(resolvedTags)])].sort();
}

function packageOwner(artifact: PackageArtifactRecord): string | null {
  for (const candidate of [
    artifact.owner_key,
    jsonPointerValue(artifact.manifest_json, "/owner"),
    jsonPointerValue(artifact.manifest_json, "/author"),
    jsonPointerValue(artifact.resolved_experiment_json, "/owner"),
  ]) {
    if (typeof candidate === "string" && candidate.trim()) {
      return candidate.trim();
    }
  }
  return null;
}

function stringArray(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.filter((item): item is string => typeof item === "string" && item.trim().length > 0);
}

function validatePackageSecretRefs(
  artifact: PackageArtifactRecord,
  secretRefs: Record<string, string>,
): void {
  const requirements = packageSecretRequirements(artifact);
  const requiredIds = new Set(requirements.map((requirement) => requirement.id));
  const missing = requirements
    .filter((requirement) => !Object.prototype.hasOwnProperty.call(secretRefs, requirement.id))
    .map((requirement) => requirement.id);
  const unknown = Object.keys(secretRefs)
    .filter((id) => !requiredIds.has(id))
    .sort();
  if (missing.length > 0 || unknown.length > 0) {
    throw new HttpError(400, "invalid_secret_refs", "Run secret refs must match the package secret requirements", {
      missing_secret_ids: missing,
      unknown_secret_ids: unknown,
      required_secret_ids: [...requiredIds].sort(),
      required_actions: missing.map((id) => ({
        action: "upload_hosted_secret",
        stage: "before_run",
        requirement_id: id,
        description: `Upload hosted secret '${id}' before creating a run, then pass --secret-ref ${id}=bucephalus://${id}.`,
        command: `buc secrets put ${id} --from-env ${id}`,
        blocking: true,
      })),
      next_commands: [
        ...missing.map((id) => `buc secrets put ${id} --from-env ${id}`),
        ...missing.map((id) => `buc run <package-digest> --secret-ref ${id}=bucephalus://${id}`),
      ],
    });
  }
}

export function packageSecretRequirements(artifact: PackageArtifactRecord): CloudSecretRequirement[] {
  const rawSecrets = jsonPointerValue(artifact.resolved_experiment_json, "/runtime/secrets");
  if (!Array.isArray(rawSecrets)) {
    return [];
  }
  const requirements: CloudSecretRequirement[] = [];
  const seen = new Set<string>();
  for (const item of rawSecrets) {
    if (!isRecord(item)) {
      continue;
    }
    const id = typeof item.name === "string" ? item.name.trim() : "";
    const mount = isRecord(item.mount) ? item.mount : null;
    const target = typeof mount?.target === "string" ? mount.target.trim() : "";
    const isEnvSecret = typeof item.from === "string" && item.from.trim() === "env";
    if (!id || (!target && !isEnvSecret) || seen.has(id)) {
      continue;
    }
    seen.add(id);
    requirements.push({
      id,
      target,
      required_for_variants: secretRequiredForVariants(item, mount ?? {}),
    });
  }
  return requirements.sort((left, right) => left.id.localeCompare(right.id));
}

function secretRequiredForVariants(secret: JsonObject, mount: JsonObject): string[] {
  const raw = Array.isArray(mount.required_for_variants)
    ? mount.required_for_variants
    : Array.isArray(secret.required_for_variants)
      ? secret.required_for_variants
      : [];
  return raw.filter((item): item is string => typeof item === "string" && item.trim().length > 0).sort();
}

function jsonPointerValue(root: JsonObject, pointer: string): unknown {
  if (pointer === "") {
    return root;
  }
  return pointer
    .split("/")
    .slice(1)
    .map((token) => token.replaceAll("~1", "/").replaceAll("~0", "~"))
    .reduce<unknown>((current, token) => {
      if (!isRecord(current)) {
        return undefined;
      }
      return current[token];
    }, root);
}

function nullableNumber(value: number | string | null): number | null {
  if (value === null) {
    return null;
  }
  return typeof value === "number" ? value : Number.parseInt(value, 10);
}

function runToWire(
  run: CloudRunRecord,
  enrichment?: RunWireEnrichment,
  options: { includeConfig?: boolean; includeRuntime?: boolean } = {},
) {
  const includeConfig = options.includeConfig ?? true;
  const includeRuntime = options.includeRuntime ?? true;
  const summary: Record<string, unknown> = {
    run_id: run.run_id,
    package_digest: run.package_digest,
    experiment_name: enrichment?.experiment_name ?? null,
    variant: runVariant(run),
    runtime: runRuntime(run),
    status: run.status,
    created_at: run.created_at,
    updated_at: run.updated_at,
  };
  if (includeConfig || run.run_label) {
    summary.run_label = run.run_label;
  }
  if (includeRuntime) {
    summary.pending_reason = isQueuedRunStatus(run.status) ? enrichment?.pending_reason ?? null : null;
    summary.trials_completed = enrichment?.trials_completed ?? null;
    summary.trials_total = enrichment?.trials_total ?? null;
  }
  if (includeConfig || run.started_at) {
    summary.started_at = run.started_at;
  }
  if (includeConfig || run.completed_at) {
    summary.completed_at = run.completed_at;
  }
  if (includeConfig || run.error_message) {
    summary.error_message = run.error_message;
  }
  if (!includeConfig) {
    return summary;
  }
  const envKeys = Object.keys(run.env ?? {}).sort();
  const secretIds = Object.keys(run.secret_refs ?? {}).sort();
  return {
    ...summary,
    env_keys: envKeys,
    secret_ids: secretIds,
    runtime_options: run.runtime_options,
    run_requirements: run.run_requirements,
    package_provenance: run.package_provenance,
  };
}

function runVariant(run: CloudRunRecord): string | null {
  for (const candidate of [
    jsonPointerValue(run.runtime_options, "/variant"),
    jsonPointerValue(run.runtime_options, "/variant_id"),
    jsonPointerValue(run.runtime_options, "/model"),
  ]) {
    if (typeof candidate === "string" && candidate.trim()) {
      return candidate.trim();
    }
  }
  return null;
}

function runRuntime(run: CloudRunRecord): string | null {
  for (const candidate of [
    jsonPointerValue(run.run_requirements as unknown as JsonObject, "/executor"),
    jsonPointerValue(run.runtime_options, "/backend"),
  ]) {
    if (typeof candidate === "string" && candidate.trim()) {
      return candidate.trim();
    }
  }
  return null;
}

function runToWorkerWire(run: CloudRunRecord) {
  return {
    run_id: run.run_id,
    package_digest: run.package_digest,
    run_label: run.run_label,
    status: run.status,
    env: run.env,
    secret_refs: run.secret_refs,
    runtime_options: run.runtime_options,
    run_requirements: run.run_requirements,
    created_at: run.created_at,
    updated_at: run.updated_at,
    started_at: run.started_at,
    completed_at: run.completed_at,
    error_message: run.error_message,
  };
}

function claimToWire(input: { run: CloudRunRecord; attempt: RunAttemptRecord }) {
  return {
    claimed: true,
    run: runToWorkerWire(input.run),
    attempt: attemptToWire(input.attempt),
  };
}

function attemptToWire(attempt: RunAttemptRecord) {
  return {
    attempt_id: attempt.attempt_id,
    run_id: attempt.run_id,
    worker_id: attempt.worker_id,
    runner_instance_id: attempt.runner_instance_id,
    status: attempt.status,
    lease_expires_at: attempt.lease_expires_at,
    heartbeat_at: attempt.heartbeat_at,
    started_at: attempt.started_at,
    ended_at: attempt.ended_at,
    error_message: attempt.error_message,
    created_at: attempt.created_at,
    updated_at: attempt.updated_at,
    ...(attempt.attempt_token ? { attempt_token: attempt.attempt_token } : {}),
  };
}

function eventToWire(event: RunEventRecord) {
  return {
    event_id: event.event_id,
    run_id: event.run_id,
    attempt_id: event.attempt_id,
    seq: typeof event.seq === "number" ? event.seq : Number.parseInt(event.seq, 10),
    event_type: event.event_type,
    payload: event.payload,
    created_at: event.created_at,
  };
}

async function requireAttemptToken(
  request: Request,
  runs: RunRepository,
  input: { attemptId: string; runnerInstanceId?: string | null; packageDigest?: string | null },
): Promise<{ runId: string; ownerKey: string | null; packageDigest: string }> {
  return await runs.verifyAttemptToken({
    ...input,
    token: requireBearer(request, "worker attempt"),
  });
}

function requireBearer(request: Request, scope: string): string {
  const authorization = request.headers.get("authorization");
  const token = authorization?.startsWith("Bearer ") ? authorization.slice("Bearer ".length) : null;
  if (!token) {
    throw new HttpError(401, "unauthorized", `${scope} requires a valid attempt token`);
  }
  return token;
}

function requireHeader(request: Request, name: string, scope: string): string {
  const value = request.headers.get(name)?.trim();
  if (!value) {
    throw new HttpError(400, "missing_header", `${scope} requires ${name}`);
  }
  return value;
}

function leaseSecondsFromBody(value: unknown): number {
  if (typeof value !== "number") {
    return 30;
  }
  return Math.min(Math.max(Math.floor(value), 5), 3600);
}

async function optionalJsonBody(request: Request): Promise<JsonObject | null> {
  const raw = await request.text();
  if (!raw.trim()) {
    return null;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new HttpError(400, "invalid_json", "Request body must be valid JSON");
  }
  return optionalJsonObject(parsed as JsonValue, "/");
}

function requireRuntimeResourceVersionPrecondition(body: Record<string, unknown> | null, operation: string): string {
  const resourceVersion = body ? optionalString(body.resource_version, "/resource_version")?.trim() : "";
  if (!resourceVersion) {
    throw new HttpError(428, "runtime_resource_version_required", `Runtime ${operation} requires resource_version from operation review`, {
      operation,
      required: ["resource_version"],
    });
  }
  return resourceVersion;
}

function requirePort(value: unknown, pointer: string): number {
  const parsed = parsePort(value);
  if (parsed === null) {
    throw new HttpError(400, "invalid_port", `${pointer} must be an integer from 1 to 65535`);
  }
  return parsed;
}

function optionalPort(value: unknown, pointer: string): number | null {
  if (value === undefined || value === null) {
    return null;
  }
  const parsed = parsePort(value);
  if (parsed === null) {
    throw new HttpError(400, "invalid_port", `${pointer} must be an integer from 1 to 65535`);
  }
  return parsed;
}

function optionalPositiveInt(value: unknown, pointer: string): number | null {
  if (value === undefined || value === null) {
    return null;
  }
  const parsed = typeof value === "number"
    ? value
    : typeof value === "string" && /^\d+$/.test(value)
      ? Number.parseInt(value, 10)
      : NaN;
  if (!Number.isInteger(parsed) || parsed < 1) {
    throw new HttpError(400, "invalid_positive_integer", `${pointer} must be a positive integer`);
  }
  return parsed;
}

function parsePort(value: unknown): number | null {
  const parsed = typeof value === "number"
    ? value
    : typeof value === "string" && /^\d+$/.test(value)
      ? Number.parseInt(value, 10)
      : NaN;
  return Number.isInteger(parsed) && parsed >= 1 && parsed <= 65535 ? parsed : null;
}

function requireStringArray(value: unknown, pointer: string): string[] {
  if (!Array.isArray(value)) {
    throw new HttpError(400, "invalid_string_array", `${pointer} must be a non-empty string array`);
  }
  const strings = value.map((item) => typeof item === "string" ? item.trim() : "");
  if (strings.length === 0 || strings.some((item) => item.length === 0)) {
    throw new HttpError(400, "invalid_string_array", `${pointer} must be a non-empty string array`);
  }
  return strings;
}

function portForwardStatusForWorkerAction(action: string): "accepted" | "active" | "completed" | "failed" | "expired" {
  switch (action) {
    case "accept":
    case "accepted":
      return "accepted";
    case "active":
      return "active";
    case "complete":
    case "completed":
      return "completed";
    case "fail":
    case "failed":
      return "failed";
    case "expire":
    case "expired":
      return "expired";
    default:
      throw new HttpError(404, "unknown_port_forward_action", "Unknown port-forward worker action");
  }
}

function execStatusForWorkerAction(action: string): "accepted" | "active" | "completed" | "failed" | "expired" {
  switch (action) {
    case "accept":
    case "accepted":
      return "accepted";
    case "active":
      return "active";
    case "complete":
    case "completed":
      return "completed";
    case "fail":
    case "failed":
      return "failed";
    case "expire":
    case "expired":
      return "expired";
    default:
      throw new HttpError(404, "unknown_exec_action", "Unknown exec worker action");
  }
}

function workerAttemptPath(pathname: string, suffix: string): boolean {
  if (!pathname.startsWith("/v1/worker/run-attempts/") || !pathname.endsWith(suffix)) {
    return false;
  }
  return !pathname.slice("/v1/worker/run-attempts/".length, -suffix.length).includes("/");
}

function attemptIdFromWorkerPath(pathname: string, suffix: string): string {
  return decodeURIComponent(pathname.slice("/v1/worker/run-attempts/".length, -suffix.length));
}

function workerPortForwardPath(pathname: string):
  | { kind: "list"; attemptId: string }
  | { kind: "update"; attemptId: string; accessRequestId: string; action: string }
  | null {
  const prefix = "/v1/worker/run-attempts/";
  if (!pathname.startsWith(prefix)) {
    return null;
  }
  const parts = pathname.slice(prefix.length).split("/").map(decodeURIComponent);
  if (parts.length === 4 && parts[1] === "runtime" && parts[2] === "resources" && parts[3] === "PortForward") {
    return { kind: "list", attemptId: parts[0] ?? "" };
  }
  if (parts.length === 6 && parts[1] === "runtime" && parts[2] === "resources" && parts[3] === "PortForward") {
    return {
      kind: "update",
      attemptId: parts[0] ?? "",
      accessRequestId: parts[4] ?? "",
      action: parts[5] ?? "",
    };
  }
  return null;
}

function workerExecPath(pathname: string):
  | { kind: "list"; attemptId: string }
  | { kind: "update"; attemptId: string; accessRequestId: string; action: string }
  | null {
  const prefix = "/v1/worker/run-attempts/";
  if (!pathname.startsWith(prefix)) {
    return null;
  }
  const parts = pathname.slice(prefix.length).split("/").map(decodeURIComponent);
  if (parts.length === 4 && parts[1] === "runtime" && parts[2] === "resources" && parts[3] === "Exec") {
    return { kind: "list", attemptId: parts[0] ?? "" };
  }
  if (parts.length === 6 && parts[1] === "runtime" && parts[2] === "resources" && parts[3] === "Exec") {
    return {
      kind: "update",
      attemptId: parts[0] ?? "",
      accessRequestId: parts[4] ?? "",
      action: parts[5] ?? "",
    };
  }
  return null;
}

function isWorkerRuntimeSubpath(pathname: string): boolean {
  const prefix = "/v1/worker/run-attempts/";
  if (!pathname.startsWith(prefix)) {
    return false;
  }
  const parts = pathname.slice(prefix.length).split("/").map(decodeURIComponent);
  return parts.length >= 3 && parts[1] === "runtime";
}

function assertRuntimeAccessBodyHasNoTarget(body: Record<string, unknown>): void {
  const fields = ["/resource_kind", "/resource_name"].filter((field) =>
    Object.prototype.hasOwnProperty.call(body, field.slice(1))
  );
  if (fields.length > 0) {
    throw new HttpError(
      400,
      "runtime_access_body_target_removed",
      "Runtime access targets are selected by the resource path; remove resource_kind/resource_name from the request body",
      { fields },
    );
  }
}

function packageContentPath(pathname: string): boolean {
  return pathname.startsWith("/v1/packages/") && pathname.endsWith("/content");
}

function packagePath(pathname: string): boolean {
  return pathname.startsWith("/v1/packages/") && !pathname.endsWith("/content");
}

function packageDigestFromContentPath(pathname: string): string {
  return decodeURIComponent(pathname.slice("/v1/packages/".length, -"/content".length));
}

function requirePackageDigest(value: unknown, pointer: string): string {
  const digest = requireString(value, pointer);
  if (!SHA256_DIGEST_PATTERN.test(digest)) {
    throw new HttpError(400, "invalid_request", `${pointer} must be sha256:<64 lowercase hex chars>`);
  }
  return digest;
}
