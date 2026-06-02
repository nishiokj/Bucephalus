import { readFile } from "node:fs/promises";
import { HttpError, jsonResponse, optionalString, readJsonObject, requireBearerToken, requireString } from "../http";
import type { JsonObject, JsonValue } from "../primitives";
import {
  optionalJsonObject,
  PackageRepository,
  requireStringMap,
  RunRepository,
  type CloudRunRecord,
  type PackageArtifactRecord,
  type RunAttemptRecord,
  type RunEventRecord,
  type RunRequirements,
} from "../packages/repository";

export async function handleRunRoute(
  request: Request,
  url: URL,
  packages: PackageRepository,
  runs: RunRepository,
  workerToken: string,
): Promise<Response | null> {
  if (request.method === "POST" && url.pathname === "/v1/worker/runs/claim") {
    requireBearerToken(request, workerToken, "worker run claim");
    return claimRun(request, runs);
  }

  if (request.method === "POST" && workerAttemptPath(url.pathname, "/heartbeat")) {
    requireBearerToken(request, workerToken, "worker attempt heartbeat");
    return heartbeatAttempt(request, url, runs);
  }

  if (request.method === "POST" && workerAttemptPath(url.pathname, "/events")) {
    requireBearerToken(request, workerToken, "worker attempt events");
    return appendRunEvent(request, url, runs);
  }

  if (request.method === "POST" && workerAttemptPath(url.pathname, "/complete")) {
    requireBearerToken(request, workerToken, "worker attempt completion");
    return completeAttempt(request, url, runs);
  }

  if (request.method === "POST" && workerAttemptPath(url.pathname, "/fail")) {
    requireBearerToken(request, workerToken, "worker attempt failure");
    return failAttempt(request, url, runs);
  }

  if (request.method === "POST" && url.pathname === "/v1/worker/runs/expire-leases") {
    requireBearerToken(request, workerToken, "worker lease expiration");
    return expireLeases(runs);
  }

  if (request.method === "GET" && packageContentPath(url.pathname)) {
    requireBearerToken(request, workerToken, "package content download");
    const digest = packageDigestFromContentPath(url.pathname);
    const artifact = await packages.getArtifact(digest);
    if (!artifact) {
      throw new HttpError(404, "package_not_found", "Package artifact not found");
    }
    if (artifact.status !== "accepted" || !artifact.storage_path) {
      throw new HttpError(409, "package_content_unavailable", "Package artifact content is unavailable");
    }
    const bytes = await readFile(artifact.storage_path);
    return new Response(bytes, {
      headers: {
        "content-type": artifact.media_type ?? "application/octet-stream",
        "content-length": String(bytes.byteLength),
        "x-bucephalus-package-digest": artifact.package_digest,
      },
    });
  }

  if (request.method === "GET" && url.pathname === "/v1/packages") {
    const limit = limitFromUrl(url);
    const artifacts = await packages.listArtifacts({ limit });
    return jsonResponse({ packages: artifacts.map(packageToWire) });
  }

  if (request.method === "GET" && packagePath(url.pathname)) {
    const digest = decodeURIComponent(url.pathname.slice("/v1/packages/".length));
    const artifact = await packages.getArtifact(digest);
    if (!artifact) {
      throw new HttpError(404, "package_not_found", "Package artifact not found");
    }
    return jsonResponse(packageToWire(artifact));
  }

  if (request.method === "POST" && url.pathname === "/v1/runs") {
    return createRun(request, packages, runs);
  }

  if (request.method === "GET" && url.pathname === "/v1/runs") {
    const limit = limitFromUrl(url);
    const records = await runs.listRuns({ limit });
    return jsonResponse({ runs: records.map(runToWire) });
  }

  if (request.method === "GET" && url.pathname.startsWith("/v1/runs/")) {
    const runId = decodeURIComponent(url.pathname.slice("/v1/runs/".length));
    const run = await runs.getRun(runId);
    if (!run) {
      throw new HttpError(404, "run_not_found", "Run not found");
    }
    return jsonResponse(runToWire(run));
  }

  return null;
}

function limitFromUrl(url: URL): number {
  const raw = url.searchParams.get("limit");
  if (!raw) {
    return 50;
  }
  const parsed = Number.parseInt(raw, 10);
  return Number.isFinite(parsed) ? parsed : 50;
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
  const attempt = await runs.heartbeatAttempt({
    attemptId: attemptIdFromWorkerPath(url.pathname, "/heartbeat"),
    runnerInstanceId: requireString(body.runner_instance_id, "/runner_instance_id"),
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
  const event = await runs.appendRunEvent({
    attemptId: attemptIdFromWorkerPath(url.pathname, "/events"),
    runnerInstanceId: requireString(body.runner_instance_id, "/runner_instance_id"),
    eventType: requireString(body.event_type, "/event_type"),
    payload: optionalJsonObject(body.payload as JsonValue | undefined, "/payload"),
  });
  return jsonResponse({ event: eventToWire(event) }, { status: 201 });
}

async function completeAttempt(
  request: Request,
  url: URL,
  runs: RunRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const result = await runs.completeAttempt({
    attemptId: attemptIdFromWorkerPath(url.pathname, "/complete"),
    runnerInstanceId: requireString(body.runner_instance_id, "/runner_instance_id"),
  });
  return jsonResponse(claimToWire(result));
}

async function failAttempt(
  request: Request,
  url: URL,
  runs: RunRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const result = await runs.failAttempt({
    attemptId: attemptIdFromWorkerPath(url.pathname, "/fail"),
    runnerInstanceId: requireString(body.runner_instance_id, "/runner_instance_id"),
    message: requireString(body.message, "/message"),
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
): Promise<Response> {
  const body = await readJsonObject(request);
  const packageDigest = requireString(body.package_digest, "/package_digest");
  const artifact = await packages.getArtifact(packageDigest);
  if (!artifact) {
    throw new HttpError(404, "package_not_found", "Package artifact not found");
  }
  if (artifact.status !== "accepted") {
    throw new HttpError(409, "package_not_runnable", "Package artifact is not accepted");
  }

  const run = await runs.createRun({
    packageDigest,
    runLabel: optionalString(body.run_label, "/run_label"),
    env: requireStringMap(body.env, "/env"),
    secretRefs: requireStringMap(body.secret_refs, "/secret_refs"),
    runtimeOptions: optionalJsonObject(body.runtime_options as JsonValue | undefined, "/runtime_options"),
    runRequirements: runRequirementsForArtifact(
      artifact,
      optionalJsonObject(body.runtime_options as JsonValue | undefined, "/runtime_options"),
    ),
  });
  return jsonResponse(runToWire(run), { status: 201 });
}

export function runRequirementsForArtifact(
  artifact: PackageArtifactRecord,
  runtimeOptions: JsonObject,
): RunRequirements {
  const requestedBackend = optionalString(runtimeOptions.backend, "/runtime_options/backend")
    ?? optionalString(runtimeOptions.executor, "/runtime_options/executor")
    ?? packageComputeBackend(artifact);
  const executor = cloudExecutorForBackend(requestedBackend);
  const imageRefs = artifact.image_refs ?? [];
  const invalidImages = imageRefs.filter((ref) => !isCloudPullableImageRef(ref));
  if (invalidImages.length > 0) {
    throw new HttpError(
      409,
      "package_images_not_cloud_pullable",
      `Package references image(s) that are not pullable from a remote registry for Cloud runs: ${invalidImages.join(", ")}`,
    );
  }
  const requires = executor === "runner-docker"
    ? ["core_runner", "docker_daemon", "registry_pull"]
    : ["core_runner", "modal", "registry_pull"];
  return {
    executor,
    requires,
    image_refs: imageRefs,
    arch: cloudArch(runtimeOptions.arch) ?? cloudArch(packageRuntimeValue(artifact, "arch")) ?? "x86_64",
    cpu_count: positiveInt(runtimeOptions.cpu_count) ?? positiveInt(runtimeOptions.cpu) ?? positiveInt(packageRuntimeValue(artifact, "cpu_count")) ?? 1,
    memory_mb: positiveInt(runtimeOptions.memory_mb) ?? positiveInt(packageRuntimeValue(artifact, "memory_mb")) ?? 1024,
    disk_mb: positiveInt(runtimeOptions.disk_mb) ?? positiveInt(packageRuntimeValue(artifact, "disk_mb")) ?? 20480,
    isolation: cloudIsolation(runtimeOptions.isolation) ?? cloudIsolation(packageRuntimeValue(artifact, "isolation")) ?? "reusable_vm",
    timeout_ms: positiveInt(runtimeOptions.timeout_ms) ?? positiveInt(packageRuntimeValue(artifact, "timeout_ms")),
    max_parallel_trials: positiveInt(runtimeOptions.max_parallel_trials)
      ?? positiveInt(packageRuntimeValue(artifact, "max_parallel_trials"))
      ?? 1,
  };
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
  const runtime = artifact.resolved_experiment_json.runtime;
  if (!isRecord(runtime)) {
    return undefined;
  }
  const compute = runtime.compute;
  if (!isRecord(compute)) {
    return undefined;
  }
  return compute[key];
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

function isCloudPullableImageRef(ref: string): boolean {
  const trimmed = ref.trim();
  if (trimmed.length === 0) {
    return false;
  }
  const firstComponent = trimmed.split("/")[0] ?? "";
  if (firstComponent === "localhost" || firstComponent.startsWith("localhost:") || firstComponent.startsWith("127.")) {
    return false;
  }
  return trimmed.includes("/") && (firstComponent.includes(".") || firstComponent.includes(":"));
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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function packageToWire(artifact: PackageArtifactRecord) {
  return {
    package_digest: artifact.package_digest,
    upload_id: artifact.upload_id,
    byte_size: nullableNumber(artifact.byte_size),
    media_type: artifact.media_type,
    manifest_json: artifact.manifest_json,
    resolved_experiment_json: artifact.resolved_experiment_json,
    target: artifact.target,
    image_refs: artifact.image_refs,
    diagnostics: artifact.diagnostics,
    status: artifact.status,
    created_at: artifact.created_at,
    updated_at: artifact.updated_at,
  };
}

function nullableNumber(value: number | string | null): number | null {
  if (value === null) {
    return null;
  }
  return typeof value === "number" ? value : Number.parseInt(value, 10);
}

function runToWire(run: CloudRunRecord) {
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
    run: runToWire(input.run),
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
