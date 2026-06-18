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

  if (request.method === "POST" && url.pathname === "/v1/worker/runs/expire-leases") {
    requireBearerToken(request, workerToken, "worker lease expiration");
    return expireLeases(runs);
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
      return jsonResponse({
        cloud_run_id: runtimeRoute.runId,
        events: runtimeEventStreamToWire(trialRows, workerEvents, limit),
      });
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

async function expireLeases(runs: RunRepository): Promise<Response> {
  const expired = await runs.expireLeases();
  return jsonResponse({
    expired: expired.map((item) => claimToWire(item)),
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
    ?? optionalString(runtimeOptions.executor, "/runtime_options/executor")
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
    cpu_count: positiveInt(runtimeOptions.cpu_count) ?? positiveInt(runtimeOptions.cpu) ?? positiveInt(packageRuntimeValue(artifact, "cpu_count")) ?? 1,
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
  "executor",
  "arch",
  "cpu_count",
  "cpu",
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
  rejectAmbiguousCloudRuntimeOptions(runtimeOptions);
}

function rejectAmbiguousCloudRuntimeOptions(runtimeOptions: JsonObject): void {
  if (
    Object.prototype.hasOwnProperty.call(runtimeOptions, "backend")
    && Object.prototype.hasOwnProperty.call(runtimeOptions, "executor")
  ) {
    throw new HttpError(
      400,
      "ambiguous_cloud_runtime_option",
      "/runtime_options/backend and /runtime_options/executor cannot both be provided; use /runtime_options/backend",
    );
  }
  if (
    Object.prototype.hasOwnProperty.call(runtimeOptions, "cpu_count")
    && Object.prototype.hasOwnProperty.call(runtimeOptions, "cpu")
  ) {
    throw new HttpError(
      400,
      "ambiguous_cloud_runtime_option",
      "/runtime_options/cpu_count and /runtime_options/cpu cannot both be provided; use /runtime_options/cpu_count",
    );
  }
}

function validateCloudRuntimeOptionValue(key: string, value: unknown): void {
  const pointer = `/runtime_options/${escapeJsonPointer(key)}`;
  if (["backend", "executor", "arch", "isolation"].includes(key)) {
    if (typeof value !== "string" || value.trim().length === 0) {
      throw new HttpError(400, "invalid_cloud_runtime_option", `${pointer} must be a non-empty string`);
    }
    return;
  }
  if (["cpu_count", "cpu", "memory_mb", "disk_mb", "timeout_ms", "max_parallel_trials"].includes(key)) {
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
    jsonPointerValue(run.runtime_options, "/executor"),
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

function workerAttemptPath(pathname: string, suffix: string): boolean {
  return pathname.startsWith("/v1/worker/run-attempts/") && pathname.endsWith(suffix);
}

function attemptIdFromWorkerPath(pathname: string, suffix: string): string {
  return decodeURIComponent(pathname.slice("/v1/worker/run-attempts/".length, -suffix.length));
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
