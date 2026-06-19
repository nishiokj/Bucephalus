import { authOwnerKey, type AuthContext } from "../auth";
import { HttpError, jsonResponse, optionalQueryInteger, optionalString, queryInteger, readJsonObject, readRequestBytes, requireBearerToken, requireString } from "../http";
import { logError, newTraceContext } from "../logging";
import { putRuntimeObject, readStoredObject } from "../objectStorage";
import { sha256Digest, type JsonObject, type JsonValue } from "../primitives";
import {
  RuntimeRepository,
  type RuntimeEventRecord,
  type RuntimeEventRowInsert,
  type RuntimeAttemptObjectContentRecord,
  type RuntimeAttemptObjectRecord,
  type RuntimeContractStageRecord,
  type RuntimeMetricObservationRecord,
  type RuntimeResults,
  type RuntimeTrialResultRecord,
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

interface RuntimeResource {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: string;
  metadata: {
    name: string;
    uid: string;
    generation: number;
    resourceVersion?: string;
    labels: Record<string, string>;
    annotations: Record<string, string>;
    ownerReferences: RuntimeResourceRef[];
    created_at?: string;
    updated_at?: string;
  };
  spec: JsonObject;
  status: JsonObject;
  audit: JsonObject;
}

interface RuntimeResourceRef {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: string;
  name: string;
  uid?: string;
}

interface RuntimeResourceInventory {
  cloud_run_id: string;
  core_run_ids: string[];
  resources: RuntimeResource[];
}

interface RuntimeResourceListWire extends RuntimeResourceInventory {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: "RuntimeResourceList";
  metadata: RuntimeCollectionMetadata;
}

interface RuntimeCollectionMetadata {
  resourceVersion: string;
  continue: string | null;
  remainingItemCount: number;
  total: number;
  returned: number;
}

interface RuntimeHealthRowWire extends Record<string, unknown> {
  health: "ready" | "degraded" | "problem" | "unknown";
  observed: "current" | "stale" | "unknown";
  access: JsonObject;
  actions: string[];
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
      return runtimeResourceResponse(url, runtimeResourceRoute, runtime);
    }
    if (request.method === "DELETE" && runtimeResourceRoute.kind === "resource-item") {
      const body = await optionalJsonBody(request);
      return jsonResponse(await runtime.cancelRuntimeAccessResource({
        run,
        resourceKind: runtimeResourceRoute.resourceKind,
        resourceName: runtimeResourceRoute.resourceName,
        resourceVersion: body ? optionalString(body.resource_version, "/resource_version") : null,
        requester: ownerKey ?? null,
        reason: body ? optionalString(body.reason, "/reason") : null,
      }));
    }
    if (request.method === "POST" && runtimeResourceRoute.kind === "resource-action") {
      const body = await optionalJsonBody(request);
      const input = {
        run,
        resourceKind: runtimeResourceRoute.resourceKind,
        resourceName: runtimeResourceRoute.resourceName,
        resourceVersion: body ? optionalString(body.resource_version, "/resource_version") : null,
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
        resourceVersion: optionalString(body.resource_version, "/resource_version"),
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
        resourceVersion: optionalString(body.resource_version, "/resource_version"),
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
    if (runtimeRoute.kind === "summary") {
      return jsonResponse(await runtime.getSummary(runtimeRoute.runId));
    }
    if (runtimeRoute.kind === "events") {
      const limit = limitFromUrl(url);
      const [trialRows, workerEvents] = await Promise.all([
        runtime.eventRows(runtimeRoute.runId, {
          limit,
          afterRowSeq: optionalIntFromUrl(url, "after_row_seq"),
        }),
        runtime.workerLifecycleEvents(runtimeRoute.runId, { limit }),
      ]);
      return jsonResponse(runtimeEventListToWire(runtimeRoute.runId, [], trialRows, workerEvents, url, limit));
    }
    if (runtimeRoute.kind === "results") {
      return jsonResponse(await runtime.results(runtimeRoute.runId, {
        limit: limitFromUrl(url),
      }));
    }
    if (runtimeRoute.kind === "artifact") {
      return runtimeArtifactContent(url, runtimeRoute, runtime);
    }
    return jsonResponse({
      cloud_run_id: runtimeRoute.runId,
      key: runtimeRoute.key,
      values: await runtime.runtimeValue(runtimeRoute.runId, runtimeRoute.key),
    });
  }

  if (request.method === "GET" && url.pathname.startsWith("/v1/runs/")) {
    const runId = decodeURIComponent(url.pathname.slice("/v1/runs/".length));
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
  | { kind: "summary"; runId: string }
  | { kind: "events"; runId: string }
  | { kind: "results"; runId: string }
  | { kind: "artifact"; runId: string; trialId: string; role: string }
  | { kind: "kv"; runId: string; key: string }
  | null {
  const prefix = "/v1/runs/";
  if (!pathname.startsWith(prefix)) {
    return null;
  }
  const parts = pathname.slice(prefix.length).split("/").map(decodeURIComponent);
  if (parts.length === 2 && parts[1] === "runtime") {
    return { kind: "summary", runId: parts[0] ?? "" };
  }
  if (parts.length === 3 && parts[1] === "runtime" && parts[2] === "events") {
    return { kind: "events", runId: parts[0] ?? "" };
  }
  if (parts.length === 3 && parts[1] === "runtime" && parts[2] === "results") {
    return { kind: "results", runId: parts[0] ?? "" };
  }
  if (parts.length === 5 && parts[1] === "runtime" && parts[2] === "artifacts") {
    return { kind: "artifact", runId: parts[0] ?? "", trialId: parts[3] ?? "", role: parts[4] ?? "" };
  }
  if (parts.length === 4 && parts[1] === "runtime" && parts[2] === "kv") {
    return { kind: "kv", runId: parts[0] ?? "", key: parts[3] ?? "" };
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

async function runtimeArtifactContent(
  url: URL,
  route: { runId: string; trialId: string; role: string },
  runtime: RuntimeRepository,
): Promise<Response> {
  const trialId = requireArtifactToken(route.trialId, "/trial_id");
  const role = requireArtifactRole(route.role, "/role");
  const attempt = optionalQueryInteger(url, "attempt", { min: 0 });
  const coreRunId = optionalString(url.searchParams.get("core_run_id"), "/core_run_id");
  if (coreRunId !== null) {
    requireCoreRunId(coreRunId, "/core_run_id");
  }
  const artifact = await runtime.attemptObjectContent(route.runId, {
    trialId,
    role,
    ...(attempt !== undefined ? { attempt } : {}),
    ...(coreRunId !== null ? { coreRunId } : {}),
  });
  if (!artifact) {
    throw new HttpError(404, "runtime_artifact_not_found", "Runtime artifact content not found");
  }
  const bytes = await readStoredObject(artifact.storage_path);
  return new Response(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer, {
    headers: {
      "content-type": artifact.media_type || "application/octet-stream",
      "content-length": String(bytes.byteLength),
      "x-bucephalus-core-run-id": artifact.core_run_id,
      "x-bucephalus-trial-id": artifact.trial_id,
      "x-bucephalus-artifact-role": artifact.role,
      "x-bucephalus-object-ref": artifact.object_ref,
      "x-bucephalus-sha256": artifact.sha256,
    },
  });
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
  runtime: RuntimeRepository,
): Promise<Response> {
  const inventory = await runtimeResourceInventoryForRun(route.runId, runtime);
  switch (route.kind) {
    case "resource-list":
      return jsonResponse(runtimeResourceListToWire(inventory, url));
    case "resource-health":
      return jsonResponse(runtimeResourceHealthToWire(inventory, url));
    case "resource-watch":
      return jsonResponse(runtimeResourceWatchToWire(inventory, url));
    case "resource-metrics-list":
      return jsonResponse(runtimeResourceMetricsListToWire(inventory, url));
    case "api-resources":
      return jsonResponse(runtimeApiResourceListToWire(inventory));
    case "api-resource": {
      const resource = runtimeApiResourceListToWire(inventory).resources
        .find((item) => runtimeApiResourceMatchesKind(item as JsonObject, route.resourceKind));
      if (!resource) {
        throw new HttpError(404, "runtime_api_resource_not_found", "Runtime API resource kind not found");
      }
      return jsonResponse(resource);
    }
    case "inspect":
      return jsonResponse(await runtimeInspectBundleToWire(route.runId, runtime, inventory, url));
    case "resource-item": {
      const resource = requireRuntimeResource(inventory, route.resourceKind, route.resourceName);
      if (url.searchParams.get("view") === "resource") {
        return jsonResponse(resource);
      }
      return jsonResponse(runtimeResourceDescribeToWire(inventory, resource, url));
    }
    case "resource-status":
      return jsonResponse(runtimeResourceStatusToWire(inventory, requireRuntimeResource(inventory, route.resourceKind, route.resourceName)));
    case "resource-events": {
      const resource = requireRuntimeResource(inventory, route.resourceKind, route.resourceName);
      const limit = limitFromUrl(url);
      const [trialRows, workerEvents] = await Promise.all([
        runtime.eventRows(route.runId, { limit, afterRowSeq: optionalIntFromUrl(url, "after_row_seq") }),
        runtime.workerLifecycleEvents(route.runId, { limit }),
      ]);
      return jsonResponse(runtimeResourceEventListToWire(inventory, resource, trialRows, workerEvents, url, limit));
    }
    case "resource-logs":
      return runtimeResourceLogsResponse(route.runId, runtime, requireRuntimeResource(inventory, route.resourceKind, route.resourceName), url);
    case "resource-content":
      return runtimeResourceContentResponse(route.runId, runtime, requireRuntimeResource(inventory, route.resourceKind, route.resourceName));
    case "resource-metrics":
      return jsonResponse(runtimeResourceMetricsToWire(
        inventory,
        requireRuntimeResource(inventory, route.resourceKind, route.resourceName),
      ));
    case "resource-operation":
      return jsonResponse(runtimeResourceOperationReviewToWire(
        route.runId,
        requireRuntimeResource(inventory, route.resourceKind, route.resourceName),
        route.operation,
      ));
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

async function runtimeResourceInventoryForRun(
  cloudRunId: string,
  runtime: RuntimeRepository,
): Promise<RuntimeResourceInventory> {
  const results = await runtime.results(cloudRunId, { limit: 1000 });
  return {
    cloud_run_id: cloudRunId,
    core_run_ids: results.core_run_ids,
    resources: runtimeResourcesFromResults(cloudRunId, results).map(withRuntimeResourceVersion),
  };
}

function runtimeResourcesFromResults(cloudRunId: string, results: RuntimeResults): RuntimeResource[] {
  return [
    ...results.trial_results.map((row) => runtimeTrialResource(cloudRunId, row)),
    ...results.metric_observations.map((row) => runtimeMetricObservationResource(cloudRunId, row)),
    ...results.contract_stages.map((row) => runtimeTrialStageResource(cloudRunId, row)),
    ...results.attempt_objects.map((row) => runtimeTrialArtifactResource(cloudRunId, row)),
  ];
}

function runtimeTrialResource(cloudRunId: string, row: RuntimeTrialResultRecord): RuntimeResource {
  const phase = runtimeTrialPhase(row);
  const ready = runtimeReadyCondition(phase, `Trial ${row.trial_id} ${phase}`, null);
  const labels = runtimeTrialLabels(cloudRunId, row);
  const updatedAt = runtimeRowTimestamp(row.row);
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "Trial",
    metadata: {
      name: runtimeTrialResourceName(row.trial_id, row.attempt),
      uid: runtimeResourceUid("Trial", cloudRunId, row.core_run_id, row.trial_id, String(row.attempt)),
      generation: 1,
      labels,
      annotations: {
        "bucephalus.dev/source": stringField(row.row.source) ?? "runtime_results",
      },
      ownerReferences: [],
      ...(updatedAt ? { updated_at: updatedAt } : {}),
    },
    spec: {
      core_run_id: row.core_run_id,
      trial_id: row.trial_id,
      schedule_idx: row.schedule_idx,
      attempt: row.attempt,
      row_seq: row.row_seq,
      variant_id: row.variant_id,
      task_id: row.task_id,
      repl_idx: row.repl_idx,
      bindings: jsonObjectOrEmpty(row.bindings),
    },
    status: {
      phase,
      outcome: row.outcome,
      primary_metric_name: row.primary_metric_name,
      primary_metric_value: row.primary_metric_value,
      metrics: row.metrics,
      events_total: row.events_total,
      has_events: row.has_events,
      row_seq: row.row_seq,
      observedGeneration: 1,
      conditions: [ready],
      access: runtimeUnavailableAccess("runtime_access_not_connected", "Runner VM access resources are not connected for this Trial yet."),
    },
    audit: {
      source: stringField(row.row.source) ?? "runtime_results",
      row: row.row,
    },
  };
}

function runtimeMetricObservationResource(cloudRunId: string, row: RuntimeMetricObservationRecord): RuntimeResource {
  const trialName = runtimeTrialResourceName(row.trial_id, row.attempt);
  const trialRef = runtimeOwnerRef("Trial", trialName, runtimeResourceUid("Trial", cloudRunId, row.core_run_id, row.trial_id, String(row.attempt)));
  const updatedAt = runtimeRowTimestamp(row.row);
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "MetricObservation",
    metadata: {
      name: runtimeResourceName(`${trialName}-${row.metric_name}-${row.row_seq}`),
      uid: runtimeResourceUid("MetricObservation", cloudRunId, row.core_run_id, row.trial_id, String(row.attempt), row.metric_name, String(row.row_seq)),
      generation: 1,
      labels: {
        ...runtimeTrialLabels(cloudRunId, row),
        "bucephalus.dev/metric-name": row.metric_name,
      },
      annotations: {
        "bucephalus.dev/source": row.metric_source ?? stringField(row.row.source) ?? "runtime_results",
      },
      ownerReferences: [trialRef],
      ...(updatedAt ? { updated_at: updatedAt } : {}),
    },
    spec: {
      core_run_id: row.core_run_id,
      trial_id: row.trial_id,
      schedule_idx: row.schedule_idx,
      attempt: row.attempt,
      row_seq: row.row_seq,
      variant_id: row.variant_id,
      task_id: row.task_id,
      repl_idx: row.repl_idx,
      metric_name: row.metric_name,
      metric_source: row.metric_source,
    },
    status: {
      phase: "recorded",
      outcome: row.outcome,
      metric_value: row.metric_value,
      observedGeneration: 1,
      conditions: [runtimeCondition("Ready", "True", "Recorded", `Metric ${row.metric_name} was recorded.`)],
    },
    audit: {
      source: row.metric_source ?? stringField(row.row.source) ?? "runtime_results",
      row: row.row,
    },
  };
}

function runtimeTrialStageResource(cloudRunId: string, row: RuntimeContractStageRecord): RuntimeResource {
  const trialName = runtimeTrialResourceName(row.trial_id, row.attempt);
  const phase = row.status || "unknown";
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "TrialStage",
    metadata: {
      name: runtimeResourceName(`${trialName}-${row.stage}`),
      uid: runtimeResourceUid("TrialStage", cloudRunId, row.core_run_id, row.trial_id, String(row.attempt), row.stage),
      generation: 1,
      labels: {
        ...runtimeTrialLabels(cloudRunId, row),
        "bucephalus.dev/stage": row.stage,
      },
      annotations: {
        "bucephalus.dev/source": stringField(row.row.source) ?? "runtime_results",
      },
      ownerReferences: [runtimeOwnerRef("Trial", trialName, runtimeResourceUid("Trial", cloudRunId, row.core_run_id, row.trial_id, String(row.attempt)))],
      ...(row.recorded_at ? { created_at: row.recorded_at, updated_at: row.recorded_at } : {}),
    },
    spec: {
      core_run_id: row.core_run_id,
      trial_id: row.trial_id,
      schedule_idx: row.schedule_idx,
      attempt: row.attempt,
      row_seq: row.row_seq,
      variant_id: row.variant_id,
      task_id: row.task_id,
      repl_idx: row.repl_idx,
      stage: row.stage,
    },
    status: {
      phase,
      status: row.status,
      detail: row.detail,
      recorded_at: row.recorded_at,
      observedGeneration: 1,
      conditions: [runtimeReadyCondition(phase, `Stage ${row.stage} ${phase}`, row.recorded_at)],
    },
    audit: {
      source: stringField(row.row.source) ?? "runtime_results",
      row: row.row,
    },
  };
}

function runtimeTrialArtifactResource(cloudRunId: string, row: RuntimeAttemptObjectRecord): RuntimeResource {
  const trialName = runtimeTrialResourceName(row.trial_id, row.attempt);
  const recordedAt = row.recorded_at_ms > 0 ? new Date(row.recorded_at_ms).toISOString() : undefined;
  const contentAvailable = row.content_available === true;
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "TrialArtifact",
    metadata: {
      name: runtimeResourceName(`${trialName}-${row.role}`),
      uid: runtimeResourceUid("TrialArtifact", cloudRunId, row.core_run_id, row.trial_id, String(row.attempt), row.role),
      generation: 1,
      labels: {
        "bucephalus.dev/run-id": cloudRunId,
        "bucephalus.dev/core-run-id": row.core_run_id,
        "bucephalus.dev/trial-id": row.trial_id,
        "bucephalus.dev/attempt": String(row.attempt),
        "bucephalus.dev/schedule-idx": String(row.schedule_idx),
        "bucephalus.dev/artifact-role": row.role,
        ...(row.sha256 ? { "bucephalus.dev/sha256": row.sha256 } : {}),
      },
      annotations: {
        "bucephalus.dev/object-ref": row.object_ref,
        ...(row.relative_path ? { "bucephalus.dev/relative-path": row.relative_path } : {}),
      },
      ownerReferences: [runtimeOwnerRef("Trial", trialName, runtimeResourceUid("Trial", cloudRunId, row.core_run_id, row.trial_id, String(row.attempt)))],
      ...(recordedAt ? { created_at: recordedAt, updated_at: recordedAt } : {}),
    },
    spec: {
      core_run_id: row.core_run_id,
      trial_id: row.trial_id,
      schedule_idx: row.schedule_idx,
      attempt: row.attempt,
      role: row.role,
      object_ref: row.object_ref,
      metadata: row.metadata ?? null,
    },
    status: {
      phase: contentAvailable ? "available" : "referenced",
      content_available: contentAvailable,
      media_type: row.media_type ?? null,
      byte_size: row.byte_size ?? null,
      sha256: row.sha256 ?? null,
      relative_path: row.relative_path ?? null,
      recorded_at_ms: row.recorded_at_ms,
      observedGeneration: 1,
      conditions: [
        runtimeCondition(
          "Ready",
          contentAvailable ? "True" : "Unknown",
          contentAvailable ? "ContentAvailable" : "ExternalReference",
          contentAvailable ? "Artifact content is persisted by Cloud." : "Artifact is referenced by runtime evidence but content is not persisted by Cloud.",
          recordedAt,
        ),
      ],
    },
    audit: {
      source: contentAvailable ? "worker_runtime_artifact_upload" : "runtime_results",
    },
  };
}

function runtimeResourceListToWire(
  inventory: RuntimeResourceInventory,
  url: URL,
): RuntimeResourceListWire {
  const filtered = filterRuntimeResources(inventory.resources, url);
  const { items, metadata } = paginateRuntimeResources(filtered, url);
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "RuntimeResourceList",
    metadata,
    cloud_run_id: inventory.cloud_run_id,
    core_run_ids: inventory.core_run_ids,
    resources: items,
  };
}

function runtimeResourceWatchToWire(inventory: RuntimeResourceInventory, url: URL): Record<string, unknown> {
  const resourceInventory = runtimeResourceListToWire(inventory, url);
  const known = knownRuntimeResourceVersions(url);
  const resourceVersions = Object.fromEntries(
    resourceInventory.resources.map((resource) => [runtimeResourceIdentity(resource), resource.metadata.resourceVersion ?? ""]),
  );
  const events = resourceInventory.resources.flatMap((resource) => {
    const identity = runtimeResourceIdentity(resource);
    const previous = known.get(identity);
    const current = resource.metadata.resourceVersion ?? "";
    if (previous && previous === current) {
      return [];
    }
    return [{
      type: previous ? "MODIFIED" : "ADDED",
      resource_ref: runtimeResourceRef(resource),
      resource_version: current,
      ...(previous ? { previous_resource_version: previous } : {}),
      resource,
    }];
  });
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "RuntimeResourceWatchList",
    cloud_run_id: inventory.cloud_run_id,
    generated_at: new Date().toISOString(),
    core_run_ids: inventory.core_run_ids,
    resource_versions: resourceVersions,
    events,
    resource_inventory: resourceInventory,
  };
}

function runtimeResourceHealthToWire(inventory: RuntimeResourceInventory, url: URL): Record<string, unknown> {
  const resources = filterRuntimeResources(inventory.resources, url);
  const rows = resources.map(runtimeResourceHealthRow);
  const summary = rows.reduce((acc, row) => {
    acc.total += 1;
    acc[row.health] += 1;
    if (Object.keys(row.access).length > 0) acc.access_targets += 1;
    if (row.access.reachable === true) acc.reachable_access_targets += 1;
    if (row.access.port_forward === true) acc.port_forward_ready += 1;
    if (row.access.exec === true) acc.exec_ready += 1;
    acc.actions_available += row.actions.length;
    acc.observed_resources += 1;
    if (row.observed === "current") acc.observed_current += 1;
    if (row.observed === "stale") acc.observed_stale += 1;
    if (row.observed === "unknown") acc.observed_unknown += 1;
    return acc;
  }, {
    total: 0,
    ready: 0,
    degraded: 0,
    problem: 0,
    unknown: 0,
    access_targets: 0,
    reachable_access_targets: 0,
    port_forward_ready: 0,
    exec_ready: 0,
    actions_available: 0,
    observed_resources: 0,
    observed_current: 0,
    observed_stale: 0,
    observed_unknown: 0,
  } satisfies Record<string, number>);
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "RuntimeResourceHealth",
    cloud_run_id: inventory.cloud_run_id,
    generated_at: new Date().toISOString(),
    core_run_ids: inventory.core_run_ids,
    summary,
    resources: rows,
  };
}

function runtimeResourceHealthRow(resource: RuntimeResource): RuntimeHealthRowWire {
  const ready = runtimeReadyConditionForResource(resource);
  const degraded = runtimeResourceConditions(resource).filter((condition) =>
    condition.type !== "Ready" && condition.status !== "True"
  );
  const health = runtimeResourceHealthState(resource, ready, degraded);
  const phase = stringField(resource.status.phase);
  const reason = stringField(resource.status.reason) ?? stringField(ready?.reason);
  const message = stringField(resource.status.message) ?? stringField(ready?.message);
  const access = jsonObjectOrEmpty(resource.status.access);
  return {
    resource: `${resource.kind}/${resource.metadata.name}`,
    resource_ref: runtimeResourceRef(resource),
    health,
    observed: runtimeResourceObserved(resource),
    phase,
    ready: ready ?? null,
    reason,
    message,
    condition_summary: ready ? `${ready.status} ${ready.reason}` : null,
    degraded_conditions: degraded,
    actions: runtimeResourceActions(resource),
    access,
    access_summary: runtimeAccessSummary(access),
    source: stringField(resource.metadata.annotations["bucephalus.dev/source"]) ?? stringField(resource.audit.source),
    updated_at: resource.metadata.updated_at ?? null,
    resource_version: resource.metadata.resourceVersion ?? null,
  };
}

function runtimeResourceStatusToWire(inventory: RuntimeResourceInventory, resource: RuntimeResource): Record<string, unknown> {
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "RuntimeResourceStatus",
    cloud_run_id: inventory.cloud_run_id,
    core_run_ids: inventory.core_run_ids,
    resource_ref: runtimeResourceRef(resource),
    generation: resource.metadata.generation ?? null,
    observedGeneration: numberField(resource.status.observedGeneration),
    resourceVersion: resource.metadata.resourceVersion ?? null,
    phase: stringField(resource.status.phase),
    reason: stringField(resource.status.reason),
    message: stringField(resource.status.message),
    conditions: runtimeResourceConditions(resource),
    actions: runtimeResourceActions(resource),
    status: resource.status,
    audit: resource.audit,
  };
}

function runtimeResourceDescribeToWire(
  inventory: RuntimeResourceInventory,
  resource: RuntimeResource,
  url: URL,
): Record<string, unknown> {
  const operations = runtimeResourceDescribeOperations(inventory.cloud_run_id, resource);
  const related = runtimeRelatedResources(inventory, resource);
  const eventLimit = queryInteger(url, "event_limit", { defaultValue: 25, min: 1, max: 250 });
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "RuntimeResourceDescribe",
    cloud_run_id: inventory.cloud_run_id,
    core_run_ids: inventory.core_run_ids,
    resource,
    operations,
    related_resources: related,
    event_list: {
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeResourceEventList",
      cloud_run_id: inventory.cloud_run_id,
      generated_at: new Date().toISOString(),
      core_run_ids: inventory.core_run_ids,
      resource,
      event_filter: runtimeEventFilterToWire(url, resource),
      metadata: runtimeEventMetadata([], eventLimit, null),
      events: [],
    },
  };
}

function runtimeResourceEventListToWire(
  inventory: RuntimeResourceInventory,
  resource: RuntimeResource,
  trialRows: RuntimeEventRecord[],
  workerEvents: WorkerLifecycleEventRecord[],
  url: URL,
  limit: number,
): Record<string, unknown> {
  const events = runtimeEventStreamToWire(trialRows, workerEvents, limit)
    .filter((event) => runtimeEventMatchesFilters(event, url, resource));
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "RuntimeResourceEventList",
    cloud_run_id: inventory.cloud_run_id,
    generated_at: new Date().toISOString(),
    core_run_ids: inventory.core_run_ids,
    resource,
    event_filter: runtimeEventFilterToWire(url, resource),
    metadata: runtimeEventMetadata(events, limit, optionalIntFromUrl(url, "after_row_seq") ?? null),
    events,
  };
}

function runtimeEventListToWire(
  cloudRunId: string,
  coreRunIds: string[],
  trialRows: RuntimeEventRecord[],
  workerEvents: WorkerLifecycleEventRecord[],
  url: URL,
  limit: number,
): Record<string, unknown> {
  const events = runtimeEventStreamToWire(trialRows, workerEvents, limit)
    .filter((event) => runtimeEventMatchesFilters(event, url));
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "RuntimeEventList",
    cloud_run_id: cloudRunId,
    generated_at: new Date().toISOString(),
    event_filter: runtimeEventFilterToWire(url),
    metadata: runtimeEventMetadata(events, limit, optionalIntFromUrl(url, "after_row_seq") ?? null),
    core_run_ids: coreRunIds,
    events,
  };
}

function runtimeEventMetadata(events: JsonObject[], limit: number, afterRowSeq: number | null): Record<string, unknown> {
  const rowSeqs = events
    .map((event) => numberField(event.row_seq))
    .filter((value): value is number => value !== null);
  const next = rowSeqs.length > 0 ? Math.max(...rowSeqs) : null;
  return {
    resourceVersion: runtimeCollectionResourceVersion(events),
    continue: null,
    remainingItemCount: null,
    limit,
    returned: events.length,
    after_row_seq: afterRowSeq,
    next_after_row_seq: next,
  };
}

function runtimeEventFilterToWire(url: URL, resource?: RuntimeResource): Record<string, unknown> {
  return {
    event_types: url.searchParams.getAll("event_type").flatMap(splitCsv).sort(),
    sources: url.searchParams.getAll("source").flatMap(splitCsv).sort(),
    resource_kind: resource?.kind ?? optionalString(url.searchParams.get("resource_kind"), "/resource_kind"),
    resource_name: resource?.metadata.name ?? optionalString(url.searchParams.get("resource_name"), "/resource_name"),
    trial_id: optionalString(url.searchParams.get("trial_id"), "/trial_id"),
    task_id: optionalString(url.searchParams.get("task_id"), "/task_id"),
  };
}

function runtimeEventMatchesFilters(event: JsonObject, url: URL, resource?: RuntimeResource): boolean {
  const types = url.searchParams.getAll("event_type").flatMap(splitCsv);
  const sources = url.searchParams.getAll("source").flatMap(splitCsv);
  if (types.length > 0 && !types.includes(String(event.event_type))) return false;
  if (sources.length > 0 && !sources.includes(String(event.source))) return false;
  const trialId = optionalString(url.searchParams.get("trial_id"), "/trial_id");
  if (trialId !== null && event.trial_id !== trialId) return false;
  const taskId = optionalString(url.searchParams.get("task_id"), "/task_id");
  if (taskId !== null && event.task_id !== taskId) return false;
  if (resource) {
    return runtimeEventReferencesResource(event, resource);
  }
  const resourceKind = optionalString(url.searchParams.get("resource_kind"), "/resource_kind");
  const resourceName = optionalString(url.searchParams.get("resource_name"), "/resource_name");
  if (resourceKind === null && resourceName === null) return true;
  return (Array.isArray(event.resource_refs) ? event.resource_refs : []).some((ref) =>
    isRecord(ref)
    && (resourceKind === null || ref.kind === resourceKind)
    && (resourceName === null || ref.name === resourceName)
  );
}

function runtimeEventReferencesResource(event: JsonObject, resource: RuntimeResource): boolean {
  if (resource.kind === "Trial" && event.trial_id === resource.spec.trial_id) return true;
  if (resource.kind === "MetricObservation" && event.trial_id === resource.spec.trial_id) return true;
  if (resource.kind === "TrialStage" && event.trial_id === resource.spec.trial_id) return true;
  if (resource.kind === "TrialArtifact" && event.trial_id === resource.spec.trial_id) return true;
  return (Array.isArray(event.resource_refs) ? event.resource_refs : []).some((ref) =>
    isRecord(ref) && ref.kind === resource.kind && ref.name === resource.metadata.name
  );
}

async function runtimeResourceLogsResponse(
  cloudRunId: string,
  runtime: RuntimeRepository,
  resource: RuntimeResource,
  url: URL,
): Promise<Response> {
  const stream = runtimeLogStream(url.searchParams.get("stream"));
  if (resource.kind !== "Trial" && resource.kind !== "TrialArtifact") {
    throw new HttpError(404, "runtime_logs_not_found", `Logs are not available for ${resource.kind}`);
  }
  const role = resource.kind === "TrialArtifact"
    ? stringField(resource.spec.role) ?? stream
    : stream;
  const artifact = await runtime.attemptObjectContent(cloudRunId, {
    trialId: requireArtifactToken(String(resource.spec.trial_id ?? ""), "/resource/spec/trial_id"),
    role,
    ...(numberField(resource.spec.attempt) !== null ? { attempt: numberField(resource.spec.attempt)! } : {}),
    ...(stringField(resource.spec.core_run_id) !== null ? { coreRunId: stringField(resource.spec.core_run_id)! } : {}),
  });
  if (!artifact) {
    throw new HttpError(404, "runtime_logs_not_found", `No ${role} runtime log content found for ${resource.kind}/${resource.metadata.name}`);
  }
  return runtimeArtifactContentResponse(artifact, {
    "x-bucephalus-log-stream": role === "stderr" ? "stderr" : "stdout",
    "x-bucephalus-resource-kind": resource.kind,
    "x-bucephalus-resource-name": resource.metadata.name,
  });
}

async function runtimeResourceContentResponse(
  cloudRunId: string,
  runtime: RuntimeRepository,
  resource: RuntimeResource,
): Promise<Response> {
  if (resource.kind !== "TrialArtifact") {
    throw new HttpError(404, "runtime_content_not_found", `Content is not available for ${resource.kind}`);
  }
  const artifact = await runtime.attemptObjectContent(cloudRunId, {
    trialId: requireArtifactToken(String(resource.spec.trial_id ?? ""), "/resource/spec/trial_id"),
    role: requireArtifactRole(String(resource.spec.role ?? ""), "/resource/spec/role"),
    ...(numberField(resource.spec.attempt) !== null ? { attempt: numberField(resource.spec.attempt)! } : {}),
    ...(stringField(resource.spec.core_run_id) !== null ? { coreRunId: stringField(resource.spec.core_run_id)! } : {}),
  });
  if (!artifact) {
    throw new HttpError(404, "runtime_content_not_found", `Runtime artifact content not found for ${resource.kind}/${resource.metadata.name}`);
  }
  return runtimeArtifactContentResponse(artifact, {
    "x-bucephalus-resource-kind": resource.kind,
    "x-bucephalus-resource-name": resource.metadata.name,
  });
}

async function runtimeArtifactContentResponse(
  artifact: RuntimeAttemptObjectContentRecord,
  headers: Record<string, string> = {},
): Promise<Response> {
  const bytes = await readStoredObject(artifact.storage_path);
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

function runtimeLogStream(value: string | null): "stdout" | "stderr" {
  if (value === null || value === "" || value === "stdout") return "stdout";
  if (value === "stderr") return "stderr";
  throw new HttpError(400, "invalid_runtime_log_stream", "/stream must be stdout or stderr");
}

function runtimeResourceMetricsListToWire(inventory: RuntimeResourceInventory, url: URL): Record<string, unknown> {
  const filtered = filterRuntimeResources(inventory.resources, url);
  const { items, metadata } = paginateRuntimeResources(filtered, url);
  const resources = items.map((resource) => runtimeResourceMetricsToWire(inventory, resource));
  const summary = resources.reduce<Record<string, number>>((acc, item) => {
    const itemSummary = item.summary as Record<string, number | undefined>;
    for (const key of [
      "metrics_total",
      "lifecycle_metrics",
      "condition_metrics",
      "access_metrics",
      "event_metrics",
      "numeric_spec_metrics",
      "numeric_status_metrics",
      "events_total",
    ]) {
      acc[key] = (acc[key] ?? 0) + (itemSummary[key] ?? 0);
    }
    return acc;
  }, {
    resources_total: filtered.length,
    resources_returned: resources.length,
    metrics_total: 0,
    lifecycle_metrics: 0,
    condition_metrics: 0,
    access_metrics: 0,
    event_metrics: 0,
    numeric_spec_metrics: 0,
    numeric_status_metrics: 0,
    events_total: 0,
  } as Record<string, number>);
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "RuntimeResourceMetricsList",
    cloud_run_id: inventory.cloud_run_id,
    generated_at: new Date().toISOString(),
    core_run_ids: inventory.core_run_ids,
    metadata,
    summary,
    resources,
  };
}

function runtimeResourceMetricsToWire(inventory: RuntimeResourceInventory, resource: RuntimeResource): Record<string, unknown> {
  const metrics = runtimeResourceMetrics(resource);
  const summary = {
    metrics_total: metrics.length,
    lifecycle_metrics: metrics.filter((metric) => metric.source === "lifecycle").length,
    condition_metrics: metrics.filter((metric) => metric.source === "condition").length,
    access_metrics: metrics.filter((metric) => metric.source === "access").length,
    event_metrics: metrics.filter((metric) => metric.source === "event").length,
    numeric_spec_metrics: metrics.filter((metric) => metric.source === "spec").length,
    numeric_status_metrics: metrics.filter((metric) => metric.source === "status").length,
    events_total: numberField(resource.status.events_total) ?? 0,
  };
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "RuntimeResourceMetrics",
    cloud_run_id: inventory.cloud_run_id,
    generated_at: new Date().toISOString(),
    core_run_ids: inventory.core_run_ids,
    resource_ref: runtimeResourceRef(resource),
    resource_version: resource.metadata.resourceVersion ?? null,
    phase: stringField(resource.status.phase),
    summary,
    metrics,
  };
}

function runtimeResourceMetrics(resource: RuntimeResource): Array<Record<string, unknown>> {
  const labels = {
    kind: resource.kind,
    name: resource.metadata.name,
    ...resource.metadata.labels,
  };
  const rows: Array<Record<string, unknown>> = [];
  const phase = stringField(resource.status.phase);
  rows.push({ name: "phase", value: phase, unit: null, source: "lifecycle", path: ".status.phase", labels });
  for (const condition of runtimeResourceConditions(resource)) {
    rows.push({
      name: `condition_${runtimeResourceName(String(condition.type))}`,
      value: condition.status === "True",
      unit: null,
      source: "condition",
      path: `.status.conditions[?type=${condition.type}]`,
      labels: { ...labels, condition: String(condition.type ?? ""), reason: String(condition.reason ?? "") },
    });
  }
  const access = jsonObjectOrEmpty(resource.status.access);
  for (const key of ["reachable", "port_forward", "exec"]) {
    if (typeof access[key] === "boolean") {
      rows.push({ name: `access_${key}`, value: access[key], unit: null, source: "access", path: `.status.access.${key}`, labels });
    }
  }
  if (numberField(resource.status.events_total) !== null) {
    rows.push({ name: "events_total", value: numberField(resource.status.events_total), unit: "events", source: "event", path: ".status.events_total", labels });
  }
  for (const [key, value] of Object.entries(resource.spec)) {
    if (typeof value === "number") {
      rows.push({ name: key, value, unit: null, source: "spec", path: `.spec.${key}`, labels });
    }
  }
  for (const [key, value] of Object.entries(resource.status)) {
    if (typeof value === "number") {
      rows.push({ name: key, value, unit: null, source: "status", path: `.status.${key}`, labels });
    }
  }
  return rows;
}

function runtimeApiResourceListToWire(inventory: RuntimeResourceInventory): Record<string, unknown> & { resources: Array<Record<string, unknown>> } {
  const byKind = new Map<string, RuntimeResource[]>();
  for (const resource of inventory.resources) {
    byKind.set(resource.kind, [...(byKind.get(resource.kind) ?? []), resource]);
  }
  const resources = runtimeApiResourceKinds().map((kind) =>
    runtimeApiResourceToWire(inventory.cloud_run_id, kind, byKind.get(kind) ?? []),
  );
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "RuntimeApiResourceList",
    cloud_run_id: inventory.cloud_run_id,
    resources,
  };
}

function runtimeApiResourceKinds(): string[] {
  return ["Trial", "MetricObservation", "TrialStage", "TrialArtifact"];
}

function runtimeApiResourceToWire(cloudRunId: string, kind: string, resources: RuntimeResource[]): Record<string, unknown> {
  const lower = kind.replace(/[A-Z]/g, (match, index) => `${index === 0 ? "" : "-"}${match.toLowerCase()}`);
  const plural = `${lower}s`;
  const subresources = ["status", "events", "metrics"];
  if (kind === "Trial" || kind === "TrialArtifact") {
    subresources.push("logs");
  }
  if (kind === "TrialArtifact") {
    subresources.push("content");
  }
  const access = kind === "Trial" ? ["port-forward", "exec"] : [];
  return {
    group: "bucephalus.dev",
    version: "v1alpha1",
    name: plural,
    singularName: lower,
    namespaced: false,
    scope: "run",
    kind,
    shortNames: runtimeApiShortNames(kind),
    categories: runtimeApiCategories(kind),
    verbs: ["list", "get", "watch"],
    subresources,
    actions: [],
    access,
    supports: {
      list: true,
      get: true,
      watch: true,
      describe: true,
      create: false,
      delete: false,
      actions: false,
      access: access.length > 0,
      labelSelector: true,
      fieldSelector: true,
    },
    pathTemplates: {
      collection: `/v1/runs/${cloudRunId}/runtime/resources?kind=${encodeURIComponent(kind)}`,
      resource: `/v1/runs/${cloudRunId}/runtime/resources/${kind}/{name}`,
      describe: `/v1/runs/${cloudRunId}/runtime/resources/${kind}/{name}`,
      operationReview: `/v1/runs/${cloudRunId}/runtime/resources/${kind}/{name}/operations/{operation}`,
      watch: `/v1/runs/${cloudRunId}/runtime/resources/watch?kind=${encodeURIComponent(kind)}`,
      subresources: Object.fromEntries(subresources.map((subresource) => [
        subresource,
        `/v1/runs/${cloudRunId}/runtime/resources/${kind}/{name}/${subresource}`,
      ])),
    },
    exampleCommands: runtimeApiExampleCommands(cloudRunId, kind),
    printerColumns: runtimeApiPrinterColumns(kind),
    fieldSelectors: ["metadata.name", "kind", "status.phase", "spec.trial_id", "spec.task_id", "spec.variant_id"],
    labelSelectors: [
      "bucephalus.dev/run-id",
      "bucephalus.dev/core-run-id",
      "bucephalus.dev/trial-id",
      "bucephalus.dev/task-id",
      "bucephalus.dev/variant-id",
      "bucephalus.dev/artifact-role",
      "bucephalus.dev/stage",
    ],
    labelSelector: true,
    count: resources.length,
    description: runtimeApiResourceDescription(kind),
  };
}

function runtimeInspectBundleToWire(
  cloudRunId: string,
  runtime: RuntimeRepository,
  inventory: RuntimeResourceInventory,
  url: URL,
): Promise<Record<string, unknown>> {
  const eventLimit = queryInteger(url, "event_limit", { defaultValue: 100, min: 1, max: 1000 });
  return Promise.all([
    runtime.eventRows(cloudRunId, { limit: eventLimit }),
    runtime.workerLifecycleEvents(cloudRunId, { limit: eventLimit }),
  ]).then(([trialRows, workerEvents]) => {
    const resourceInventory = runtimeResourceListToWire(inventory, url);
    const resourceHealth = runtimeResourceHealthToWire(inventory, url);
    const resourceMetrics = runtimeResourceMetricsListToWire(inventory, url);
    return {
      apiVersion: "bucephalus.dev/v1alpha1",
      kind: "RuntimeInspectBundle",
      cloud_run_id: cloudRunId,
      generated_at: new Date().toISOString(),
      resource_filter: {
        kinds: url.searchParams.getAll("kind").flatMap(splitCsv),
        label_selector: optionalString(url.searchParams.get("label_selector"), "/label_selector"),
        field_selector: optionalString(url.searchParams.get("field_selector"), "/field_selector"),
      },
      api_resources: runtimeApiResourceListToWire(inventory),
      resource_inventory: resourceInventory,
      resource_health: resourceHealth,
      resource_metrics: resourceMetrics,
      event_list: runtimeEventListToWire(cloudRunId, inventory.core_run_ids, trialRows, workerEvents, url, eventLimit),
      log_refs: inventory.resources
        .filter((resource) => resource.kind === "Trial" || resource.kind === "TrialArtifact")
        .map((resource) => ({
          resource: runtimeResourceRef(resource),
          streams: ["stdout", "stderr"],
          urls: {
            stdout: `/v1/runs/${cloudRunId}/runtime/resources/${resource.kind}/${encodeURIComponent(resource.metadata.name)}/logs?stream=stdout`,
            stderr: `/v1/runs/${cloudRunId}/runtime/resources/${resource.kind}/${encodeURIComponent(resource.metadata.name)}/logs?stream=stderr`,
          },
        })),
    };
  });
}

function runtimeResourceOperationReviewToWire(
  cloudRunId: string,
  resource: RuntimeResource,
  operation: string,
): Record<string, unknown> {
  const normalized = operation.trim();
  const base = runtimeResourceOperationFor(cloudRunId, resource, normalized);
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: "RuntimeResourceOperationReview",
    cloud_run_id: cloudRunId,
    resource_ref: runtimeResourceRef(resource),
    resource_version: resource.metadata.resourceVersion ?? "",
    resource_generation: resource.metadata.generation ?? null,
    observed_generation: numberField(resource.status.observedGeneration),
    operation: normalized,
    matched_operation: stringField(base.purpose),
    supported: base.supported === true,
    reason: stringField(base.reason),
    message: stringField(base.message),
    command: stringField(base.command),
    verb: stringField(base.verb),
    subresource: stringField(base.subresource),
    action: stringField(base.action),
    requires_active_run: base.requires_active_run === true,
  };
}

function runtimeResourceDescribeOperations(cloudRunId: string, resource: RuntimeResource): Array<Record<string, unknown>> {
  return [
    "get",
    "status",
    "events",
    ...(resource.kind === "Trial" || resource.kind === "TrialArtifact" ? ["logs/stdout", "logs/stderr"] : []),
    ...(resource.kind === "TrialArtifact" ? ["content"] : []),
    ...(resource.kind === "Trial" ? ["port-forward", "exec"] : []),
  ].map((operation) => runtimeResourceOperationFor(cloudRunId, resource, operation));
}

function runtimeResourceOperationFor(cloudRunId: string, resource: RuntimeResource, operation: string): Record<string, unknown> {
  const resourcePath = `/v1/runs/${cloudRunId}/runtime/resources/${resource.kind}/${encodeURIComponent(resource.metadata.name)}`;
  const commandPrefix = `bucephalus-cloud run`;
  const target = `${resource.kind}/${resource.metadata.name}`;
  switch (operation) {
    case "get":
      return runtimeOperation("get", `${commandPrefix} resource get ${cloudRunId} ${target}`, true, "get", null, null, false);
    case "status":
      return runtimeOperation("status", `${commandPrefix} resource status ${cloudRunId} ${target}`, true, "get", "status", null, false);
    case "events":
      return runtimeOperation("events", `${commandPrefix} resource events ${cloudRunId} ${target}`, true, "get", "events", null, false);
    case "logs":
    case "logs/stdout":
      return runtimeOperation("logs/stdout", `${commandPrefix} logs ${cloudRunId} ${target} --stream stdout`, runtimeResourceSupportsLogs(resource), "get", "logs", null, false);
    case "logs/stderr":
      return runtimeOperation("logs/stderr", `${commandPrefix} logs ${cloudRunId} ${target} --stream stderr`, runtimeResourceSupportsLogs(resource), "get", "logs", null, false);
    case "content":
      return runtimeOperation("content", `${commandPrefix} resource content ${cloudRunId} ${target}`, resource.kind === "TrialArtifact", "get", "content", null, false);
    case "port-forward":
      return {
        ...runtimeOperation(
          "port-forward",
          `${commandPrefix} port-forward ${cloudRunId} ${target} --target-port PORT`,
          false,
          "create",
          "port-forward",
          null,
          true,
        ),
        reason: "runtime_access_not_connected",
        message: "Port-forward access resources are modeled in the API contract, but worker-side tunnel creation is not connected yet.",
      };
    case "exec":
      return {
        ...runtimeOperation(
          "exec",
          `${commandPrefix} exec ${cloudRunId} ${target} -- COMMAND [ARG...]`,
          false,
          "create",
          "exec",
          null,
          true,
        ),
        reason: "runtime_access_not_connected",
        message: "Exec access resources are modeled in the API contract, but worker-side command execution is not connected yet.",
      };
    default:
      return {
        ...runtimeOperation(operation, `${resourcePath}/operations/${encodeURIComponent(operation)}`, false, null, null, null, false),
        reason: "unknown_operation",
        message: `Runtime operation '${operation}' is not supported for ${resource.kind}.`,
      };
  }
}

function runtimeOperation(
  purpose: string,
  command: string,
  supported: boolean,
  verb: string | null,
  subresource: string | null,
  action: string | null,
  requiresActiveRun: boolean,
): Record<string, unknown> {
  return {
    purpose,
    command,
    supported,
    reason: supported ? null : "unsupported_runtime_operation",
    message: supported ? null : `Operation '${purpose}' is not supported for this resource.`,
    verb,
    subresource,
    action,
    requires_active_run: requiresActiveRun,
  };
}

function runtimeResourceSupportsLogs(resource: RuntimeResource): boolean {
  return resource.kind === "Trial" || resource.kind === "TrialArtifact";
}

function runtimeApiResourceMatchesKind(resource: JsonObject, kind: string): boolean {
  const wanted = kind.trim().toLowerCase();
  const candidates = [
    resource.kind,
    resource.name,
    resource.singularName,
    ...(Array.isArray(resource.shortNames) ? resource.shortNames : []),
  ].map((item) => String(item).toLowerCase());
  return candidates.includes(wanted);
}

function runtimeApiShortNames(kind: string): string[] {
  switch (kind) {
    case "Trial":
      return ["trial", "tr"];
    case "MetricObservation":
      return ["metric", "mo"];
    case "TrialStage":
      return ["stage", "ts"];
    case "TrialArtifact":
      return ["artifact", "ta"];
    default:
      return [];
  }
}

function runtimeApiCategories(kind: string): string[] {
  switch (kind) {
    case "Trial":
      return ["workload", "result"];
    case "MetricObservation":
      return ["result", "metric"];
    case "TrialStage":
      return ["lifecycle", "debug"];
    case "TrialArtifact":
      return ["artifact", "debug"];
    default:
      return ["runtime"];
  }
}

function runtimeApiExampleCommands(cloudRunId: string, kind: string): JsonObject[] {
  const target = `${kind}/{name}`;
  return [
    { purpose: "list", command: `bucephalus-cloud run resources ${cloudRunId} --kind ${kind}` },
    { purpose: "get", command: `bucephalus-cloud run resource get ${cloudRunId} ${target}` },
    { purpose: "events", command: `bucephalus-cloud run resource events ${cloudRunId} ${target}` },
    ...(kind === "Trial" || kind === "TrialArtifact"
      ? [{ purpose: "logs", command: `bucephalus-cloud run logs ${cloudRunId} ${target} --stream stdout` }]
      : []),
    ...(kind === "Trial"
      ? [
        { purpose: "port-forward", command: `bucephalus-cloud run port-forward ${cloudRunId} ${target} --target-port PORT` },
        { purpose: "exec", command: `bucephalus-cloud run exec ${cloudRunId} ${target} -- COMMAND [ARG...]` },
      ]
      : []),
  ];
}

function runtimeApiPrinterColumns(kind: string): JsonObject[] {
  const base = [
    { name: "Name", type: "string", jsonPath: ".metadata.name", description: "Resource name", priority: 0 },
    { name: "Phase", type: "string", jsonPath: ".status.phase", description: "Runtime phase", priority: 0 },
    { name: "Age", type: "date", jsonPath: ".metadata.created_at", description: "Creation timestamp", priority: 1 },
  ];
  switch (kind) {
    case "Trial":
      return [
        ...base,
        { name: "Task", type: "string", jsonPath: ".spec.task_id", description: "Task id", priority: 0 },
        { name: "Variant", type: "string", jsonPath: ".spec.variant_id", description: "Variant id", priority: 0 },
        { name: "Outcome", type: "string", jsonPath: ".status.outcome", description: "Trial outcome", priority: 0 },
      ];
    case "MetricObservation":
      return [
        ...base,
        { name: "Metric", type: "string", jsonPath: ".spec.metric_name", description: "Metric name", priority: 0 },
        { name: "Value", type: "string", jsonPath: ".status.metric_value", description: "Metric value", priority: 0 },
      ];
    case "TrialStage":
      return [
        ...base,
        { name: "Stage", type: "string", jsonPath: ".spec.stage", description: "Contract stage", priority: 0 },
        { name: "Status", type: "string", jsonPath: ".status.status", description: "Stage status", priority: 0 },
      ];
    case "TrialArtifact":
      return [
        ...base,
        { name: "Role", type: "string", jsonPath: ".spec.role", description: "Artifact role", priority: 0 },
        { name: "Bytes", type: "integer", jsonPath: ".status.byte_size", description: "Artifact byte size", priority: 0 },
      ];
    default:
      return base;
  }
}

function runtimeApiResourceDescription(kind: string): string {
  switch (kind) {
    case "Trial":
      return "A scheduled experiment trial and its observed outcome, metrics, lifecycle, and access posture.";
    case "MetricObservation":
      return "A single metric emitted by a Trial.";
    case "TrialStage":
      return "A contract-stage lifecycle record for a Trial.";
    case "TrialArtifact":
      return "An artifact reference or persisted artifact object produced by a Trial.";
    default:
      return "Runtime resource.";
  }
}

function runtimeRelatedResources(inventory: RuntimeResourceInventory, resource: RuntimeResource): Array<Record<string, unknown>> {
  const resourceRef = runtimeResourceRef(resource);
  const owners = resource.metadata.ownerReferences.flatMap((owner) => {
    const found = inventory.resources.find((candidate) =>
      candidate.kind === owner.kind && candidate.metadata.name === owner.name
    );
    return found ? [{ relationship: "owner", resource: found }] : [];
  });
  const dependents = inventory.resources
    .filter((candidate) => candidate.metadata.ownerReferences.some((owner) =>
      owner.kind === resourceRef.kind && owner.name === resourceRef.name
    ))
    .map((candidate) => ({ relationship: "dependent", resource: candidate }));
  return [...owners, ...dependents];
}

function requireRuntimeResource(inventory: RuntimeResourceInventory, kind: string, name: string): RuntimeResource {
  const resource = inventory.resources.find((candidate) =>
    candidate.kind === kind && candidate.metadata.name === name
  );
  if (!resource) {
    throw new HttpError(404, "runtime_resource_not_found", `Runtime resource ${kind}/${name} not found`);
  }
  return resource;
}

function filterRuntimeResources(resources: RuntimeResource[], url: URL): RuntimeResource[] {
  const kinds = new Set(url.searchParams.getAll("kind").flatMap(splitCsv));
  const labelSelectors = parseRuntimeSelectors(url.searchParams.get("label_selector"));
  const fieldSelectors = parseRuntimeSelectors(url.searchParams.get("field_selector"));
  return resources.filter((resource) => {
    if (kinds.size > 0 && !kinds.has(resource.kind)) return false;
    for (const [key, value] of labelSelectors) {
      if (resource.metadata.labels[key] !== value) return false;
    }
    for (const [key, value] of fieldSelectors) {
      if (runtimeFieldSelectorValue(resource, key) !== value) return false;
    }
    return true;
  });
}

function paginateRuntimeResources(resources: RuntimeResource[], url: URL): { items: RuntimeResource[]; metadata: RuntimeCollectionMetadata } {
  const limit = limitFromUrl(url);
  const offset = runtimeContinueOffset(url.searchParams.get("continue"));
  const items = resources.slice(offset, offset + limit);
  const nextOffset = offset + items.length;
  const remaining = Math.max(0, resources.length - nextOffset);
  return {
    items,
    metadata: {
      resourceVersion: runtimeCollectionResourceVersion(resources),
      continue: remaining > 0 ? String(nextOffset) : null,
      remainingItemCount: remaining,
      total: resources.length,
      returned: items.length,
    },
  };
}

function runtimeContinueOffset(value: string | null): number {
  if (value === null || value === "") return 0;
  if (!/^[0-9]+$/.test(value)) {
    throw new HttpError(400, "invalid_query", "/continue must be a non-negative integer cursor");
  }
  return Number.parseInt(value, 10);
}

function parseRuntimeSelectors(value: string | null): Array<[string, string]> {
  if (!value?.trim()) return [];
  return value.split(",").flatMap((part) => {
    const [rawKey, rawValue] = part.split("=", 2);
    const key = rawKey?.trim();
    const selectorValue = rawValue?.trim();
    if (!key || selectorValue === undefined) {
      throw new HttpError(400, "invalid_runtime_selector", "Runtime selectors must use key=value clauses");
    }
    return [[key, selectorValue] as [string, string]];
  });
}

function runtimeFieldSelectorValue(resource: RuntimeResource, key: string): string | undefined {
  if (key === "kind") return resource.kind;
  if (key === "metadata.name") return resource.metadata.name;
  if (key === "metadata.uid") return resource.metadata.uid;
  if (key.startsWith("metadata.labels.")) return resource.metadata.labels[key.slice("metadata.labels.".length)];
  if (key.startsWith("spec.")) return primitiveString(jsonPathValue(resource.spec, key.slice("spec.".length)));
  if (key.startsWith("status.")) return primitiveString(jsonPathValue(resource.status, key.slice("status.".length)));
  return undefined;
}

function jsonPathValue(root: JsonObject, path: string): unknown {
  return path.split(".").reduce<unknown>((current, segment) => {
    if (!isRecord(current)) return undefined;
    return current[segment];
  }, root);
}

function primitiveString(value: unknown): string | undefined {
  if (value === null || value === undefined) return undefined;
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return undefined;
}

function splitCsv(value: string): string[] {
  return value.split(",").map((item) => item.trim()).filter(Boolean);
}

function knownRuntimeResourceVersions(url: URL): Map<string, string> {
  const out = new Map<string, string>();
  for (const item of url.searchParams.getAll("known_resource")) {
    const [identity, version] = item.split("=", 2);
    if (identity && version) out.set(identity, version);
  }
  return out;
}

function withRuntimeResourceVersion(resource: RuntimeResource): RuntimeResource {
  const stable = {
    ...resource,
    metadata: {
      ...resource.metadata,
      resourceVersion: undefined,
    },
  };
  const resourceVersion = sha256Digest(JSON.stringify(stable)).slice("sha256:".length, "sha256:".length + 16);
  return {
    ...resource,
    metadata: {
      ...resource.metadata,
      resourceVersion,
    },
  };
}

function runtimeCollectionResourceVersion(values: unknown[]): string {
  return sha256Digest(JSON.stringify(values)).slice("sha256:".length, "sha256:".length + 16);
}

function runtimeTrialLabels(
  cloudRunId: string,
  row: Pick<RuntimeTrialResultRecord, "core_run_id" | "trial_id" | "schedule_idx" | "attempt" | "variant_id" | "task_id" | "repl_idx">,
): Record<string, string> {
  return {
    "bucephalus.dev/run-id": cloudRunId,
    "bucephalus.dev/core-run-id": row.core_run_id,
    "bucephalus.dev/trial-id": row.trial_id,
    "bucephalus.dev/schedule-idx": String(row.schedule_idx),
    "bucephalus.dev/attempt": String(row.attempt),
    "bucephalus.dev/variant-id": row.variant_id,
    "bucephalus.dev/task-id": row.task_id,
    "bucephalus.dev/repl-idx": String(row.repl_idx),
  };
}

function runtimeTrialPhase(row: RuntimeTrialResultRecord): string {
  const outcome = row.outcome.trim().toLowerCase();
  if (["success", "succeeded", "pass", "passed"].includes(outcome)) return "succeeded";
  if (["failure", "failed", "error", "errored"].includes(outcome)) return "failed";
  if (["timeout", "timed_out"].includes(outcome)) return "failed";
  return outcome || "unknown";
}

function runtimeReadyCondition(phase: string, message: string, timestamp: string | null): JsonObject {
  const normalized = phase.toLowerCase();
  if (["succeeded", "available", "recorded", "ok", "passed", "success"].includes(normalized)) {
    return runtimeCondition("Ready", "True", "Completed", message, timestamp ?? undefined);
  }
  if (["failed", "error", "errored", "timeout", "cancelled"].includes(normalized)) {
    return runtimeCondition("Ready", "False", "Failed", message, timestamp ?? undefined);
  }
  return runtimeCondition("Ready", "Unknown", "Observed", message, timestamp ?? undefined);
}

function runtimeCondition(
  type: string,
  status: "True" | "False" | "Unknown",
  reason: string,
  message: string,
  lastTransitionTime?: string,
): JsonObject {
  return {
    type,
    status,
    reason,
    message,
    ...(lastTransitionTime ? { lastTransitionTime } : {}),
  };
}

function runtimeReadyConditionForResource(resource: RuntimeResource): JsonObject | null {
  return runtimeResourceConditions(resource).find((condition) => condition.type === "Ready") ?? null;
}

function runtimeResourceConditions(resource: RuntimeResource): JsonObject[] {
  return Array.isArray(resource.status.conditions)
    ? resource.status.conditions.filter((item): item is JsonObject => isRecord(item))
    : [];
}

function runtimeResourceHealthState(
  resource: RuntimeResource,
  ready: JsonObject | null,
  degraded: JsonObject[],
): "ready" | "degraded" | "problem" | "unknown" {
  if (ready?.status === "True" && degraded.length === 0) return "ready";
  if (ready?.status === "False") return "problem";
  if (degraded.length > 0) return "degraded";
  const phase = String(resource.status.phase ?? "").toLowerCase();
  if (["failed", "error", "errored"].includes(phase)) return "problem";
  if (["running", "pending", "referenced"].includes(phase)) return "degraded";
  return "unknown";
}

function runtimeResourceObserved(resource: RuntimeResource): "current" | "stale" | "unknown" {
  const generation = numberField(resource.metadata.generation);
  const observed = numberField(resource.status.observedGeneration);
  if (generation === null || observed === null) return "unknown";
  return generation === observed ? "current" : "stale";
}

function runtimeResourceActions(_resource: RuntimeResource): string[] {
  return [];
}

function runtimeAccessSummary(access: JsonObject): string | null {
  if (Object.keys(access).length === 0) return null;
  const parts = [
    access.reachable === true ? "reachable" : "not reachable",
    access.port_forward === true ? "port-forward" : "",
    access.exec === true ? "exec" : "",
    stringField(access.reason),
  ].filter((item): item is string => Boolean(item));
  return parts.join(", ");
}

function runtimeUnavailableAccess(reason: string, message: string): JsonObject {
  return {
    reachable: false,
    reason,
    message,
    port_forward: false,
    exec: false,
  };
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

function runtimeResourceRef(resource: RuntimeResource): RuntimeResourceRef {
  return {
    apiVersion: "bucephalus.dev/v1alpha1",
    kind: resource.kind,
    name: resource.metadata.name,
    uid: resource.metadata.uid,
  };
}

function runtimeResourceIdentity(resource: RuntimeResource): string {
  return `${resource.apiVersion}/${resource.kind}/${resource.metadata.name}`;
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
  limit: number,
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
  return [...workerWire, ...trialWire]
    .sort((a, b) => sortKey(a).localeCompare(sortKey(b)))
    .slice(0, limit) as JsonObject[];
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

function portForwardStatusForWorkerAction(action: string): "accepted" | "active" | "failed" | "expired" {
  switch (action) {
    case "accept":
    case "accepted":
      return "accepted";
    case "active":
      return "active";
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
