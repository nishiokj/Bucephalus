import { isIP } from "node:net";
import { authOwnerKey, type AuthContext } from "../auth";
import { decodePathParam, HttpError, jsonResponse, optionalString, queryIntegerParam, readJsonObject, requireBearerToken, requireString } from "../http";
import { isObjectStorageBoundaryError, readStoredObject } from "../objectStorage";
import type { JsonObject, JsonValue } from "../primitives";
import {
  publicBoundaryImportDiagnostic,
  publicBoundaryJsonObject,
  publicBoundaryText,
  publicBoundaryValue,
} from "../publicBoundary";
import { RuntimeRepository } from "../runtime/repository";
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
} from "../packages/repository";
import {
  allowsControlPlaneSecretRefs,
  controlPlaneSecretIdViolation,
  controlPlaneSecretRefViolation,
} from "../secrets/policy";

const SHA256_DIGEST_PATTERN = /^sha256:[0-9a-f]{64}$/;
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

interface CloudSecretRequirement {
  id: string;
  target: string;
  required_for_variants: string[];
}

export async function handleRunRoute(
  request: Request,
  url: URL,
  packages: PackageRepository,
  runs: RunRepository,
  runtime: RuntimeRepository,
  workerToken: string,
  auth?: AuthContext | null,
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
    const digest = requireSha256Digest(packageDigestFromContentPath(url.pathname), "/package_digest");
    await requireAttemptToken(request, runs, {
      attemptId: requireUuidString(
        requireHeader(request, "x-bucephalus-attempt-id", "package content download"),
        "/attempt_id",
      ),
      packageDigest: digest,
    });
    const artifact = await packages.getArtifact(digest);
    if (!artifact) {
      throw new HttpError(404, "package_not_found", "Package artifact not found");
    }
    if (artifact.status !== "accepted" || !artifact.storage_path) {
      throw new HttpError(409, "package_content_unavailable", "Package artifact content is unavailable");
    }
    const bytes = await readPackageArtifactContent(artifact.storage_path);
    return new Response(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer, {
      headers: {
        "content-type": artifact.media_type ?? "application/octet-stream",
        "content-length": String(bytes.byteLength),
        "x-bucephalus-package-digest": artifact.package_digest,
      },
    });
  }

  if (request.method === "GET" && url.pathname === "/v1/packages") {
    const limit = limitFromUrl(url, 200);
    const artifacts = await packages.listArtifacts({ limit, ownerKey });
    return jsonResponse({ packages: artifacts.map(packageToWire) });
  }

  if (request.method === "GET" && packagePath(url.pathname)) {
    const digest = requireSha256Digest(
      decodePathParam(url.pathname.slice("/v1/packages/".length), "/package_digest"),
      "/package_digest",
    );
    const artifact = await packages.getArtifact(digest, ownerKey);
    if (!artifact) {
      throw new HttpError(404, "package_not_found", "Package artifact not found");
    }
    return jsonResponse(packageToWire(artifact));
  }

  if (request.method === "POST" && url.pathname === "/v1/runs") {
    return createRun(request, packages, runs, ownerKey);
  }

  if (request.method === "GET" && url.pathname === "/v1/runs") {
    const limit = limitFromUrl(url, 200);
    const records = await runs.listRuns({ limit, ownerKey });
    return jsonResponse({ runs: records.map(runToWire) });
  }

  const runtimeRoute = runtimePath(url.pathname);
  if (request.method === "GET" && runtimeRoute) {
    const run = await runs.getRun(runtimeRoute.runId, ownerKey);
    if (!run) {
      throw new HttpError(404, "run_not_found", "Run not found");
    }
    if (runtimeRoute.kind === "summary") {
      return jsonResponse(runtimeToWire(await runtime.getSummary(runtimeRoute.runId)));
    }
    if (runtimeRoute.kind === "events") {
      return jsonResponse(runtimeToWire({
        cloud_run_id: runtimeRoute.runId,
        events: await runtime.eventRows(runtimeRoute.runId, {
          limit: limitFromUrl(url, 1000),
          afterRowSeq: optionalIntFromUrl(url, "after_row_seq"),
        }),
      }));
    }
    if (runtimeRoute.kind === "results") {
      return jsonResponse(runtimeToWire(await runtime.results(runtimeRoute.runId, {
        limit: limitFromUrl(url, 1000),
      })));
    }
    return jsonResponse(runtimeToWire({
      cloud_run_id: runtimeRoute.runId,
      key: runtimeRoute.key,
      values: await runtime.runtimeValue(runtimeRoute.runId, runtimeRoute.key),
    }));
  }

  if (request.method === "GET" && url.pathname.startsWith("/v1/runs/")) {
    const runId = requireUuidString(
      decodePathParam(url.pathname.slice("/v1/runs/".length), "/run_id"),
      "/run_id",
    );
    const run = await runs.getRun(runId, ownerKey);
    if (!run) {
      throw new HttpError(404, "run_not_found", "Run not found");
    }
    return jsonResponse(runToWire(run));
  }

  return null;
}

async function readPackageArtifactContent(storagePath: string): Promise<Uint8Array> {
  try {
    return await readStoredObject(storagePath);
  } catch (error) {
    if (isObjectStorageBoundaryError(error)) {
      throw new HttpError(409, "package_content_unavailable", "Package artifact content is unavailable");
    }
    throw error;
  }
}

function runtimeToWire<T>(value: T): T {
  return publicBoundaryValue(value) as T;
}

function limitFromUrl(url: URL, max: number): number {
  return queryIntegerParam(url, "limit", { defaultValue: 50, min: 1, max });
}

function optionalIntFromUrl(url: URL, key: string): number | undefined {
  return queryIntegerParam(url, key, { min: 0, max: Number.MAX_SAFE_INTEGER });
}

function requireSha256Digest(value: unknown, pointer: string): string {
  const digest = requireString(value, pointer);
  if (!SHA256_DIGEST_PATTERN.test(digest)) {
    throw new HttpError(400, "invalid_digest", `${pointer} must be sha256:<64 lowercase hex chars>`);
  }
  return digest;
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

function runtimePath(pathname: string):
  | { kind: "summary"; runId: string }
  | { kind: "events"; runId: string }
  | { kind: "results"; runId: string }
  | { kind: "kv"; runId: string; key: string }
  | null {
  const prefix = "/v1/runs/";
  if (!pathname.startsWith(prefix)) {
    return null;
  }
  const parts = pathname.slice(prefix.length).split("/").map((part, index) =>
    decodePathParam(part, index === 0 ? "/run_id" : index === 3 ? "/runtime/key" : "/path")
  );
  if (parts.length === 2 && parts[1] === "runtime") {
    return { kind: "summary", runId: requireUuidString(parts[0], "/run_id") };
  }
  if (parts.length === 3 && parts[1] === "runtime" && parts[2] === "events") {
    return { kind: "events", runId: requireUuidString(parts[0], "/run_id") };
  }
  if (parts.length === 3 && parts[1] === "runtime" && parts[2] === "results") {
    return { kind: "results", runId: requireUuidString(parts[0], "/run_id") };
  }
  if (parts.length === 4 && parts[1] === "runtime" && parts[2] === "kv") {
    return { kind: "kv", runId: requireUuidString(parts[0], "/run_id"), key: parts[3] ?? "" };
  }
  return null;
}

async function claimRun(request: Request, runs: RunRepository): Promise<Response> {
  const body = await readJsonObject(request);
  const runnerInstanceId = requireUuidString(body.runner_instance_id, "/runner_instance_id");
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
  const runnerInstanceId = requireUuidString(body.runner_instance_id, "/runner_instance_id");
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
  const runnerInstanceId = requireUuidString(body.runner_instance_id, "/runner_instance_id");
  await requireAttemptToken(request, runs, { attemptId, runnerInstanceId });
  const payload = optionalJsonObject(body.payload as JsonValue | undefined, "/payload");
  const event = await runs.appendRunEvent({
    attemptId,
    runnerInstanceId,
    eventType: publicBoundaryText(requireString(body.event_type, "/event_type")),
    payload: publicBoundaryJsonObject(payload) ?? {},
  });
  return jsonResponse({ event: eventToWire(event) }, { status: 201 });
}

async function completeAttempt(
  request: Request,
  url: URL,
  runs: RunRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const attemptId = attemptIdFromWorkerPath(url.pathname, "/complete");
  const runnerInstanceId = requireUuidString(body.runner_instance_id, "/runner_instance_id");
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
  const runnerInstanceId = requireUuidString(body.runner_instance_id, "/runner_instance_id");
  await requireAttemptToken(request, runs, { attemptId, runnerInstanceId });
  const result = await runs.failAttempt({
    attemptId,
    runnerInstanceId,
    message: publicBoundaryText(requireString(body.message, "/message")),
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
  ownerKey?: string,
): Promise<Response> {
  const body = await readJsonObject(request);
  const packageDigest = requireSha256Digest(body.package_digest, "/package_digest");
  const artifact = await packages.getArtifact(packageDigest, ownerKey);
  if (!artifact) {
    throw new HttpError(404, "package_not_found", "Package artifact not found");
  }
  if (artifact.status !== "accepted") {
    throw new HttpError(409, "package_not_runnable", "Package artifact is not accepted");
  }
  if (!artifact.storage_path) {
    throw new HttpError(409, "package_content_unavailable", "Package artifact content is unavailable");
  }

  const secretRefs = cloudSecretRefs(requireStringMap(body.secret_refs, "/secret_refs"));
  const env = requireStringMap(body.env, "/env");
  validateCloudRunPlainEnv(env, body.allow_secret_env);
  const runtimeOptions = optionalJsonObject(body.runtime_options as JsonValue | undefined, "/runtime_options");
  validatePackageSecretRefs(artifact, secretRefs);

  const run = await runs.createRun({
    packageDigest,
    runLabel: optionalString(body.run_label, "/run_label"),
    env,
    secretRefs,
    runtimeOptions,
    ownerKey,
    runRequirements: runRequirementsForArtifact(
      artifact,
      runtimeOptions,
      secretRefs,
    ),
  });
  return jsonResponse(runToWire(run), { status: 201 });
}

function validateCloudRunPlainEnv(env: Record<string, string>, allowSecretEnv: unknown): void {
  const invalidKeys = Object.keys(env).filter((key) => !/^[A-Z_][A-Z0-9_]*$/.test(key)).sort();
  if (invalidKeys.length > 0) {
    throw new HttpError(400, "invalid_cloud_env_key", `Invalid Cloud run env key '${invalidKeys[0]}'`);
  }
  if (allowSecretEnv === true) {
    return;
  }
  if (allowSecretEnv !== undefined && allowSecretEnv !== false) {
    throw new HttpError(400, "invalid_allow_secret_env", "/allow_secret_env must be a boolean");
  }
  const secretLike = Object.keys(env).filter((key) => secretLikeEnvKey(key)).sort();
  if (secretLike.length === 0) {
    return;
  }
  throw new HttpError(
    400,
    "secret_like_plain_env",
    `Cloud run env contains secret-looking key(s): ${secretLike.join(", ")}. Plain env values are sent to the Cloud API as run configuration. Use secret_refs for provider secrets, or set allow_secret_env=true only when every listed value is intentionally non-secret.`,
  );
}

function secretLikeEnvKey(key: string): boolean {
  const normalized = key.toUpperCase();
  const compact = normalized.replaceAll(/[^A-Z0-9]/g, "");
  if (["APIKEY", "ACCESSKEY", "PRIVATEKEY"].some((needle) => compact.includes(needle))) {
    return true;
  }
  return normalized
    .split(/[^A-Z0-9]+/)
    .filter((part) => part.length > 0)
    .some((part) => [
      "SECRET",
      "TOKEN",
      "PASSWORD",
      "PASSWD",
      "CREDENTIAL",
      "CREDENTIALS",
      "OAUTH",
    ].includes(part));
}

export function runRequirementsForArtifact(
  artifact: PackageArtifactRecord,
  runtimeOptions: JsonObject,
  secretRefs: Record<string, string> = {},
): RunRequirements {
  const packageExecutor = cloudExecutorForBackend(packageComputeBackend(artifact));
  const executor = cloudExecutorForRuntimeOptions(runtimeOptions)
    ?? packageExecutor;
  validateCoreLaunchRuntimeOptions(runtimeOptions);
  rejectUnsupportedCloudTrialRuntime(artifact, runtimeOptions);
  const imageRefs = [...new Set([
    ...(artifact.image_refs ?? []),
    ...packageRuntimeImageRefs(artifact),
    ...runtimeOptionsRuntimeImageRefs(runtimeOptions),
  ])].sort();
  const invalidImages = process.env.BUCEPHALUS_CLOUD_ALLOW_LOCAL_IMAGE_REFS === "true"
    ? []
    : imageRefs.filter((ref) => !isCloudDigestPinnedImageRef(ref));
  if (invalidImages.length > 0) {
    throw new HttpError(
      409,
      "package_images_not_cloud_pinned",
      `Cloud run references image(s) that are not digest-pinned remote registry refs: ${invalidImages.join(", ")}`,
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
  if (networkPerimeter.egress_hosts.length > 0 && (executor === "modal" || packageExecutor === "modal")) {
    throw new HttpError(
      400,
      "unsupported_cloud_network_perimeter",
      "modal Cloud runs do not support network perimeter requirements; use runner-docker or remove runtime.network.egress and allowlist modes",
    );
  }
  if (networkPerimeter.egress_hosts.length > 0) {
    requires.push("network_perimeter");
  }
  const sidecars = [...new Set([
    ...packageTrialSidecars(artifact),
    ...cloudRuntimeRequirementAliases(runtimeOptions.sidecars, "/runtime_options/sidecars"),
  ])].sort();
  if (executor === "modal" && sidecars.length > 0) {
    throw new HttpError(
      400,
      "unsupported_cloud_sidecars",
      "modal Cloud runs do not support sidecars; use runner-docker or remove trial_runtime sidecars",
    );
  }
  const accelerators = [...new Set([
    ...cloudRuntimeRequirementAliases(
      packagePolicyTaskSandboxResourceValue(artifact, "accelerators"),
      "/policy/task_sandbox/resources/accelerators",
    ),
    ...cloudRuntimeRequirementAliases(packageRuntimeValue(artifact, "accelerators"), "/runtime/compute/accelerators"),
    ...cloudRuntimeRequirementAliases(runtimeOptions.accelerators, "/runtime_options/accelerators"),
  ])].sort();
  if (executor === "modal" && accelerators.length > 0) {
    throw new HttpError(
      400,
      "unsupported_cloud_accelerators",
      "modal Cloud runs do not support accelerator requirements; use runner-docker or remove runtime accelerator requirements",
    );
  }
  for (const sidecar of sidecars) {
    requires.push(`sidecar:${sidecar}`);
  }
  for (const accelerator of accelerators) {
    requires.push(`accelerator:${accelerator}`);
  }
  const packageArch = cloudArch(packageRuntimeValue(artifact, "arch"), "/runtime/compute/arch");
  const packagePolicyCpuCount = optionalPositiveInt(packagePolicyTaskSandboxResourceValue(artifact, "cpu_count"), "/policy/task_sandbox/resources/cpu_count");
  const packageRuntimeCpuCount = optionalPositiveInt(packageRuntimeValue(artifact, "cpu_count"), "/runtime/compute/cpu_count");
  const packagePolicyMemoryMb = optionalPositiveInt(packagePolicyTaskSandboxResourceValue(artifact, "memory_mb"), "/policy/task_sandbox/resources/memory_mb");
  const packageRuntimeMemoryMb = optionalPositiveInt(packageRuntimeValue(artifact, "memory_mb"), "/runtime/compute/memory_mb");
  const packageDiskMb = optionalPositiveInt(packageRuntimeValue(artifact, "disk_mb"), "/runtime/compute/disk_mb");
  const packageIsolation = cloudIsolation(packageRuntimeValue(artifact, "isolation"), "/runtime/compute/isolation");
  const packagePolicyTimeoutMs = optionalPositiveInt(packagePolicyValue(artifact, "timeout_ms"), "/policy/timeout_ms");
  const packageRuntimeTimeoutMs = optionalPositiveInt(packageRuntimeValue(artifact, "timeout_ms"), "/runtime/compute/timeout_ms");
  const packageSchedulingMaxConcurrency = optionalPositiveInt(packageSchedulingValue(artifact, "max_concurrency"), "/scheduling/max_concurrency");
  const packageComputeMaxParallel = optionalPositiveInt(packageComputeConfigValue(artifact, "max_parallel"), "/runtime/compute/config/max_parallel");
  const packageRuntimeMaxParallelTrials = optionalPositiveInt(packageRuntimeValue(artifact, "max_parallel_trials"), "/runtime/compute/max_parallel_trials");
  return {
    executor,
    requires: [...new Set(requires)],
    image_refs: imageRefs,
    secret_ids: secretIds,
    network_perimeter: networkPerimeter,
    sidecars,
    accelerators,
    arch: cloudArch(runtimeOptions.arch, "/runtime_options/arch")
      ?? packageArch
      ?? "x86_64",
    cpu_count: optionalPositiveInt(runtimeOptions.cpu_count, "/runtime_options/cpu_count")
      ?? optionalPositiveInt(runtimeOptions.cpu, "/runtime_options/cpu")
      ?? packagePolicyCpuCount
      ?? packageRuntimeCpuCount
      ?? 1,
    memory_mb: optionalPositiveInt(runtimeOptions.memory_mb, "/runtime_options/memory_mb")
      ?? packagePolicyMemoryMb
      ?? packageRuntimeMemoryMb
      ?? 1024,
    disk_mb: optionalPositiveInt(runtimeOptions.disk_mb, "/runtime_options/disk_mb")
      ?? packageDiskMb
      ?? 20480,
    isolation: cloudIsolation(runtimeOptions.isolation, "/runtime_options/isolation")
      ?? packageIsolation
      ?? "reusable_vm",
    timeout_ms: optionalPositiveInt(runtimeOptions.timeout_ms, "/runtime_options/timeout_ms")
      ?? packagePolicyTimeoutMs
      ?? packageRuntimeTimeoutMs,
    max_parallel_trials: optionalPositiveInt(runtimeOptions.max_parallel_trials, "/runtime_options/max_parallel_trials")
      ?? packageSchedulingMaxConcurrency
      ?? packageComputeMaxParallel
      ?? packageRuntimeMaxParallelTrials
      ?? 1,
  };
}

function cloudNetworkPerimeter(
  artifact: PackageArtifactRecord,
  runtimeOptions: JsonObject,
): RunRequirements["network_perimeter"] {
  const packageNetwork = cloudNetworkConfig(packageRuntimeTopLevelValue(artifact, "network"), "/runtime/network");
  const packageExternalApis = packageExternalApiEgressHosts(artifact);
  const runtimeNetwork = cloudNetworkConfig(runtimeOptions.network, "/runtime_options/network");
  if (!packageNetwork && packageExternalApis.length === 0 && !runtimeNetwork) {
    return {
      default: "none",
      task_sandbox: "none",
      agent: "none",
      egress_hosts: [],
    };
  }

  const defaultMode = mergeNetworkMode(packageNetwork?.default, runtimeNetwork?.default);
  const taskSandboxMode = mergeNetworkMode(packageNetwork?.task_sandbox, runtimeNetwork?.task_sandbox);
  const agentMode = mergeNetworkMode(packageNetwork?.agent, runtimeNetwork?.agent);
  const egressHosts = [...new Set([
    ...(packageNetwork?.egress_hosts ?? []),
    ...packageExternalApis,
    ...(runtimeNetwork?.egress_hosts ?? []),
  ])].sort();
  const hasAllowlistMode = [defaultMode, taskSandboxMode, agentMode].includes("allowlist_enforced");
  if (hasAllowlistMode && egressHosts.length === 0) {
    throw new HttpError(
      400,
      "unsupported_cloud_network_egress",
      "runtime.network.egress must declare at least one hostname when a Cloud network mode is allowlist_enforced",
    );
  }

  return {
    default: defaultMode,
    task_sandbox: taskSandboxMode,
    agent: agentMode,
    egress_hosts: egressHosts,
  };
}

type CloudNetworkConfig = {
  default: RunNetworkMode;
  task_sandbox: RunNetworkMode;
  agent: RunNetworkMode;
  egress_hosts: string[];
};

function cloudNetworkConfig(value: unknown, pointer: string): CloudNetworkConfig | null {
  const network = optionalObject(value, pointer);
  if (!network) {
    return null;
  }
  const defaultMode = cloudNetworkMode(network.default, `${pointer}/default`);
  return {
    default: defaultMode ?? "none",
    task_sandbox: cloudNetworkMode(network.task_sandbox, `${pointer}/task_sandbox`) ?? defaultMode ?? "none",
    agent: cloudNetworkMode(network.agent, `${pointer}/agent`) ?? defaultMode ?? "none",
    egress_hosts: cloudEgressHosts(network.egress, `${pointer}/egress`),
  };
}

function packageExternalApiEgressHosts(artifact: PackageArtifactRecord): string[] {
  const runtime = packageRuntimeObject(artifact);
  const runtimeExternals = optionalObject(runtime.externals, "/runtime/externals");
  const topLevelExternals = optionalObject(artifact.resolved_experiment_json.externals, "/externals");
  if (runtimeExternals && topLevelExternals && !sameJsonValue(runtimeExternals, topLevelExternals)) {
    throw new HttpError(
      400,
      "invalid_cloud_runtime_shape",
      "/externals conflicts with another Cloud runtime alias for 'externals'",
    );
  }
  const externals = runtimeExternals ?? topLevelExternals;
  if (!externals) {
    return [];
  }
  return cloudEgressHosts(externals.apis, runtimeExternals ? "/runtime/externals/apis" : "/externals/apis");
}

function mergeNetworkMode(left: RunNetworkMode | undefined, right: RunNetworkMode | undefined): RunNetworkMode {
  return left === "allowlist_enforced" || right === "allowlist_enforced"
    ? "allowlist_enforced"
    : "none";
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

function cloudEgressHosts(value: unknown, pointer: string): string[] {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new HttpError(400, "unsupported_cloud_network_egress", `${pointer} must be an array of hostnames`);
  }
  const hosts = value.map((item) => {
    if (typeof item !== "string" || item.trim().length === 0) {
      throw new HttpError(400, "unsupported_cloud_network_egress", `${pointer} entries must be non-empty hostnames`);
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
  const { hostname, port } = splitEgressHost(host);
  if (isIP(hostname) !== 0 || !validCloudHostname(hostname) || !validCloudEgressPort(port)) {
    throw new HttpError(400, "unsupported_cloud_network_egress", `Unsupported Cloud egress host '${value}'`);
  }
  return host;
}

function splitEgressHost(host: string): { hostname: string; port: string | null } {
  const portSeparator = host.lastIndexOf(":");
  if (portSeparator === -1) {
    return { hostname: host, port: null };
  }
  return {
    hostname: host.slice(0, portSeparator),
    port: host.slice(portSeparator + 1),
  };
}

function validCloudHostname(hostname: string): boolean {
  if (hostname.length === 0 || hostname.length > 253 || hostname.startsWith(".") || hostname.endsWith(".")) {
    return false;
  }
  return hostname.split(".").every((label) => (
    label.length > 0
    && label.length <= 63
    && /^[a-z0-9-]+$/.test(label)
    && !label.startsWith("-")
    && !label.endsWith("-")
  ));
}

function validCloudEgressPort(port: string | null): boolean {
  if (port === null) {
    return true;
  }
  const parsed = Number.parseInt(port, 10);
  return Number.isInteger(parsed) && parsed >= 1 && parsed <= 65535;
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
        `Unsupported Cloud secret ref for '${id}'. Use gcp-secret-manager://... or aws-secrets-manager://...`,
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
  return validGcpSecretManagerRef(ref) || validAwsSecretsManagerRef(ref);
}

function validGcpSecretManagerRef(ref: string): boolean {
  const prefix = "gcp-secret-manager://";
  if (!ref.startsWith(prefix)) {
    return false;
  }
  const match = /^projects\/([^/]+)\/secrets\/([^/]+)\/versions\/([^/]+)$/.exec(ref.slice(prefix.length));
  if (!match) {
    return false;
  }
  const [, project, secret, version] = match;
  return /^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(project ?? "")
    && /^[A-Za-z0-9_-]+$/.test(secret ?? "")
    && /^(latest|[0-9]+)$/.test(version ?? "");
}

function validAwsSecretsManagerRef(ref: string): boolean {
  const prefix = "aws-secrets-manager://";
  if (!ref.startsWith(prefix)) {
    return false;
  }
  const secretId = ref.slice(prefix.length);
  return /^[A-Za-z0-9/_+=.@-]+$/.test(secretId);
}

function rejectUnsupportedCloudTrialRuntime(
  artifact: PackageArtifactRecord,
  runtimeOptions: JsonObject,
): void {
  const runtimeAgentSite = trialRuntimeAgentSite(runtimeOptions, "/runtime_options/trial_runtime");
  const packageTrial = {
    trial_runtime: packageTrialRuntime(artifact) as JsonObject,
  } satisfies JsonObject;
  const packageAgentSite = trialRuntimeAgentSite(packageTrial, "/trial_runtime");
  if (packageAgentSite === "host" || runtimeAgentSite === "host") {
    throw new HttpError(
      400,
      "unsupported_cloud_agent_site",
      "Cloud runs do not support trial_runtime.execution.agent_site=host",
    );
  }
  validateCloudGraderStrategy(
    trialRuntimeGraderStrategy(packageTrial, "/trial_runtime"),
  );
  validateCloudGraderStrategy(
    trialRuntimeGraderStrategy(runtimeOptions, "/runtime_options/trial_runtime"),
  );
  rejectRuntimeOptionTrialSidecars(runtimeOptions);
}

function trialRuntimeAgentSite(value: JsonObject, pointer: string): string | null {
  const trialRuntime = optionalObject(value.trial_runtime, pointer);
  if (!trialRuntime) {
    return null;
  }
  const execution = optionalObject(trialRuntime.execution, `${pointer}/execution`);
  if (!execution) {
    return null;
  }
  if (execution.agent_site !== undefined && typeof execution.agent_site !== "string") {
    throw new HttpError(400, "invalid_cloud_runtime_shape", `${pointer}/execution/agent_site must be a string`);
  }
  if (typeof execution.agent_site !== "string") {
    return null;
  }
  return execution.agent_site.trim();
}

function trialRuntimeGraderStrategy(value: JsonObject, pointer: string): string | null {
  const trialRuntime = optionalObject(value.trial_runtime, pointer);
  if (!trialRuntime) {
    return null;
  }
  const grader = optionalObject(trialRuntime.grader, `${pointer}/grader`);
  if (!grader) {
    return null;
  }
  if (grader.strategy !== undefined && typeof grader.strategy !== "string") {
    throw new HttpError(400, "invalid_cloud_runtime_shape", `${pointer}/grader/strategy must be a string`);
  }
  if (typeof grader.strategy !== "string") {
    return null;
  }
  return grader.strategy.trim();
}

function validateCloudGraderStrategy(strategy: string | null): void {
  if (!strategy) {
    return;
  }
  switch (strategy) {
    case "none":
    case "in_task_runtime":
    case "injected":
    case "separate":
      return;
    case "host":
      throw new HttpError(
        400,
        "unsupported_cloud_grader_strategy",
        "Cloud runs do not support trial_runtime.grader.strategy=host",
      );
    default:
      throw new HttpError(
        400,
        "unsupported_cloud_grader_strategy",
        `Unsupported Cloud grader strategy '${strategy}'`,
      );
  }
}

function rejectRuntimeOptionTrialSidecars(runtimeOptions: JsonObject): void {
  const trialRuntime = optionalObject(runtimeOptions.trial_runtime, "/runtime_options/trial_runtime");
  if (!trialRuntime) {
    return;
  }
  for (const stage of ["agent", "grader"]) {
    const stageConfig = optionalObject(trialRuntime[stage], `/runtime_options/trial_runtime/${stage}`);
    for (const key of ["sidecars", "ephemerals"]) {
      if (stageConfig?.[key] === undefined) {
        continue;
      }
      throw new HttpError(
        400,
        "unsupported_cloud_runtime_option",
        `/runtime_options/trial_runtime/${stage}/${key} is not supported for Cloud run creation; declare sidecars in the package YAML or use runtime_options.sidecars for Cloud runner requirement aliases`,
      );
    }
  }
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

function cloudRuntimeRequirementAliases(value: unknown, pointer: string): string[] {
  return cloudStringList(value, pointer).map((item) => {
    if (!portableCloudRequirementAlias(item)) {
      throw new HttpError(
        400,
        "unsupported_cloud_runtime_requirement",
        `${pointer} entries must be portable Cloud requirement aliases`,
      );
    }
    return item;
  });
}

function portableCloudRequirementAlias(value: string): boolean {
  return /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(value);
}

function packageComputeBackend(artifact: PackageArtifactRecord): string {
  const compute = packageRuntimeCompute(artifact);
  if (compute) {
    return optionalString(compute.backend, "/runtime/compute/backend") ?? "runner-docker";
  }
  return "runner-docker";
}

function packageRuntimeValue(artifact: PackageArtifactRecord, key: string): unknown {
  const compute = packageRuntimeCompute(artifact);
  if (!compute) {
    return undefined;
  }
  return compute[key];
}

function packageComputeConfigValue(artifact: PackageArtifactRecord, key: string): unknown {
  const compute = packageRuntimeCompute(artifact);
  if (!compute) {
    return undefined;
  }
  const config = optionalObject(compute.config, "/runtime/compute/config");
  return config?.[key];
}

function packageRuntimeTopLevelValue(artifact: PackageArtifactRecord, key: string): unknown {
  const runtime = packageRuntimeObject(artifact);
  return runtime[key];
}

function packagePolicyValue(artifact: PackageArtifactRecord, key: string): unknown {
  const policy = optionalObject(artifact.resolved_experiment_json.policy, "/policy");
  if (!policy) {
    return undefined;
  }
  return policy[key];
}

function packagePolicyTaskSandboxResourceValue(artifact: PackageArtifactRecord, key: string): unknown {
  const policy = optionalObject(artifact.resolved_experiment_json.policy, "/policy");
  if (!policy) {
    return undefined;
  }
  const taskSandbox = optionalObject(policy.task_sandbox, "/policy/task_sandbox");
  if (!taskSandbox) {
    return undefined;
  }
  const resources = optionalObject(taskSandbox.resources, "/policy/task_sandbox/resources");
  return resources?.[key];
}

function packageSchedulingValue(artifact: PackageArtifactRecord, key: string): unknown {
  const scheduling = optionalObject(artifact.resolved_experiment_json.scheduling, "/scheduling");
  if (!scheduling) {
    return undefined;
  }
  return scheduling[key];
}

function packageRuntimeObject(artifact: PackageArtifactRecord): Record<string, unknown> {
  return optionalObject(artifact.resolved_experiment_json.runtime, "/runtime") ?? {};
}

function packageRuntimeCompute(artifact: PackageArtifactRecord): Record<string, unknown> | null {
  return optionalObject(packageRuntimeObject(artifact).compute, "/runtime/compute");
}

function optionalObject(value: unknown, pointer: string): Record<string, unknown> | null {
  if (value === undefined) {
    return null;
  }
  if (!isRecord(value)) {
    throw new HttpError(400, "invalid_cloud_runtime_shape", `${pointer} must be an object`);
  }
  return value;
}

function packageTrialSidecars(artifact: PackageArtifactRecord): string[] {
  const declaredIds = packageDeclaredSidecarIds(artifact);
  const ids = [
    ...packageStageSidecars(artifact, "agent"),
    ...packageStageSidecars(artifact, "grader"),
  ];
  const out: string[] = [];
  for (const id of ids) {
    if (!declaredIds.has(id)) {
      throw new HttpError(
        400,
        "unsupported_cloud_sidecars",
        `trial_runtime sidecar '${id}' is referenced but not declared`,
      );
    }
    out.push(id);
  }
  return [...new Set(out)].sort();
}

function packageDeclaredSidecarIds(artifact: PackageArtifactRecord): Set<string> {
  const ids = new Set<string>();
  for (const [id, value] of Object.entries(packageDeclaredSidecars(artifact))) {
    if (!/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(id)) {
      throw new HttpError(
        400,
        "unsupported_cloud_sidecars",
        `/sidecars/${id} id must be a portable runtime alias: lowercase letters, numbers, and '-' only; it must start and end with a letter or number`,
      );
    }
    const config = optionalObject(value, `/sidecars/${id}`);
    if (typeof config?.image !== "string" || config.image.trim().length === 0) {
      throw new HttpError(400, "unsupported_cloud_sidecars", `/sidecars/${id} image is required`);
    }
    if (config.lifecycle !== "per-trial") {
      throw new HttpError(400, "unsupported_cloud_sidecars", `/sidecars/${id} lifecycle must be per-trial`);
    }
    ids.add(id);
  }
  return ids;
}

function packageDeclaredSidecars(artifact: PackageArtifactRecord): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  mergeAliasedObject(out, optionalObject(artifact.resolved_experiment_json.sidecars, "/sidecars"), "/sidecars");
  mergeAliasedObject(out, optionalObject(artifact.resolved_experiment_json.ephemerals, "/ephemerals"), "/ephemerals");
  return out;
}

function mergeAliasedObject(target: Record<string, unknown>, source: Record<string, unknown> | null, pointer: string): void {
  if (!source) {
    return;
  }
  for (const [key, value] of Object.entries(source)) {
    const existing = target[key];
    if (existing !== undefined && !sameJsonValue(existing, value)) {
      const conflictPointer = pointer.length === 0 ? `/${key}` : `${pointer}/${key}`;
      throw new HttpError(
        400,
        "invalid_cloud_runtime_shape",
        `${conflictPointer} conflicts with another Cloud runtime alias for '${key}'`,
      );
    }
    target[key] = value;
  }
}

function runtimeOptionsRuntimeImageRefs(runtimeOptions: JsonObject): string[] {
  return runtimeImageRefs(runtimeOptions, "/runtime_options/trial_runtime");
}

function runtimeImageRefs(
  value: JsonObject,
  pointer: string,
  options: { allowRuntimeFieldSource?: boolean } = {},
): string[] {
  const refs: string[] = [];
  const trialRuntime = optionalObject(value.trial_runtime, pointer);
  if (trialRuntime) {
    const agent = optionalObject(trialRuntime.agent, `${pointer}/agent`);
    const agentImage = runtimeImageRefValue(agent?.image, `${pointer}/agent/image`);
    if (agentImage) {
      refs.push(agentImage);
    }
    const grader = optionalObject(trialRuntime.grader, `${pointer}/grader`);
    const separate = optionalObject(grader?.separate, `${pointer}/grader/separate`);
    const graderImage = runtimeImageRefValue(separate?.image, `${pointer}/grader/separate/image`);
    if (graderImage) {
      refs.push(graderImage);
    }
    const task = optionalObject(trialRuntime.task, `${pointer}/task`);
    const workspace = optionalObject(task?.workspace, `${pointer}/task/workspace`);
    const workspaceImage = runtimeImageRefValue(workspace?.image, `${pointer}/task/workspace/image`, options);
    if (workspaceImage) {
      refs.push(workspaceImage);
    }
  }
  return refs;
}

function packageRuntimeImageRefs(artifact: PackageArtifactRecord): string[] {
  const refs = runtimeImageRefs({
    trial_runtime: packageTrialRuntime(artifact) as JsonObject,
  } satisfies JsonObject, "/trial_runtime", { allowRuntimeFieldSource: true });
  packageDeclaredSidecarIds(artifact);
  for (const [id, value] of Object.entries(packageDeclaredSidecars(artifact))) {
    const config = optionalObject(value, `/sidecars/${id}`);
    if (typeof config?.image === "string" && config.image.trim().length > 0) {
      refs.push(config.image.trim());
    }
  }
  return refs;
}

function runtimeImageRefValue(
  value: unknown,
  pointer: string,
  options: { allowRuntimeFieldSource?: boolean } = {},
): string | null {
  if (value === undefined) {
    return null;
  }
  if (options.allowRuntimeFieldSource && isRuntimeFieldSource(value)) {
    return null;
  }
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new HttpError(400, "invalid_cloud_runtime_shape", `${pointer} must be a non-empty image ref string`);
  }
  return value.trim();
}

function isRuntimeFieldSource(value: unknown): boolean {
  return isRecord(value)
    && Object.keys(value).length === 1
    && value.from === "case_row";
}

function packageStageSidecars(artifact: PackageArtifactRecord, stage: string): string[] {
  const trialRuntime = packageTrialRuntime(artifact);
  if (Object.keys(trialRuntime).length === 0) {
    return [];
  }
  const stageConfig = optionalObject(trialRuntime[stage], `/trial_runtime/${stage}`);
  if (!stageConfig) {
    return [];
  }
  return cloudStringList(stageConfig.sidecars, `/trial_runtime/${stage}/sidecars`);
}

function packageTrialRuntime(artifact: PackageArtifactRecord): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  mergeAliasedObject(out, optionalObject(artifact.resolved_experiment_json.trial_runtime, "/trial_runtime"), "/trial_runtime");
  for (const stage of ["task", "agent", "execution", "grader"]) {
    const value = artifact.resolved_experiment_json[stage];
    if (value !== undefined) {
      mergeAliasedObject(out, { [stage]: value }, "");
    }
  }
  const stages = optionalObject(artifact.resolved_experiment_json.stages, "/stages");
  if (stages) {
    for (const [stage, value] of Object.entries(stages)) {
      const target = stage === "case" ? "task" : stage;
      mergeAliasedObject(out, { [target]: normalizeStageEphemerals(value, `/stages/${stage}`) }, `/stages/${stage}`);
    }
  }
  for (const stage of ["agent", "grader"]) {
    const existing = out[stage];
    if (existing !== undefined) {
      out[stage] = normalizeStageEphemerals(existing, `/trial_runtime/${stage}`);
    }
  }
  return out;
}

function normalizeStageEphemerals(value: unknown, pointer: string): unknown {
  if (value === undefined) {
    return undefined;
  }
  const stage = optionalObject(value, pointer);
  if (!stage) {
    return value;
  }
  if (stage.ephemerals === undefined) {
    return stage;
  }
  if (stage.sidecars !== undefined && !sameJsonValue(stage.sidecars, stage.ephemerals)) {
    throw new HttpError(
      400,
      "invalid_cloud_runtime_shape",
      `${pointer}/ephemerals conflicts with ${pointer}/sidecars`,
    );
  }
  return {
    ...stage,
    sidecars: stage.sidecars ?? stage.ephemerals,
  };
}

function sameJsonValue(left: unknown, right: unknown): boolean {
  return stableJsonValue(left) === stableJsonValue(right);
}

function stableJsonValue(value: unknown): string {
  if (Array.isArray(value)) {
    return `[${value.map(stableJsonValue).join(",")}]`;
  }
  if (isRecord(value)) {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJsonValue(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
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

function cloudExecutorForRuntimeOptions(runtimeOptions: JsonObject): RunRequirements["executor"] | null {
  const backend = optionalString(runtimeOptions.backend, "/runtime_options/backend");
  const executor = optionalString(runtimeOptions.executor, "/runtime_options/executor");
  const backendExecutor = backend ? cloudExecutorForBackend(backend) : null;
  const optionExecutor = executor ? cloudExecutorForBackend(executor) : null;
  if (backendExecutor && optionExecutor && backendExecutor !== optionExecutor) {
    throw new HttpError(
      400,
      "conflicting_cloud_executor",
      "runtime_options.backend and runtime_options.executor select different Cloud executors",
    );
  }
  return backendExecutor ?? optionExecutor;
}

function validateCoreLaunchRuntimeOptions(runtimeOptions: JsonObject): void {
  if (runtimeOptions.smoke_test !== undefined && typeof runtimeOptions.smoke_test !== "boolean") {
    throw new HttpError(400, "unsupported_cloud_runtime_option", "/runtime_options/smoke_test must be a boolean");
  }
  const materialize = optionalString(runtimeOptions.materialize, "/runtime_options/materialize");
  if (!materialize) {
    return;
  }
  switch (materialize.trim()) {
    case "none":
    case "metadata-only":
    case "metadata_only":
    case "outputs-only":
    case "outputs_only":
    case "full":
      return;
    default:
      throw new HttpError(400, "unsupported_cloud_runtime_option", `Unsupported Core materialize mode '${materialize}'`);
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

function cloudArch(value: unknown, pointer: string): RunRequirements["arch"] | null {
  const arch = optionalString(value, pointer);
  if (!arch) {
    return null;
  }
  switch (arch.trim().toLowerCase()) {
    case "x86_64":
    case "amd64":
      return "x86_64";
    case "arm64":
    case "aarch64":
      return "arm64";
    default:
      throw new HttpError(400, "unsupported_cloud_arch", `Unsupported Cloud runner architecture '${arch}'`);
  }
}

function cloudIsolation(value: unknown, pointer: string): RunRequirements["isolation"] | null {
  const isolation = optionalString(value, pointer);
  if (!isolation) {
    return null;
  }
  switch (isolation.trim()) {
    case "reusable_vm":
    case "single_use_vm":
      return isolation.trim() as RunRequirements["isolation"];
    default:
      throw new HttpError(400, "unsupported_cloud_isolation", `Unsupported Cloud isolation mode '${isolation}'`);
  }
}

function optionalPositiveInt(value: unknown, pointer: string): number | null {
  if (value === undefined || value === null) {
    return null;
  }
  if (typeof value === "number" && Number.isSafeInteger(value) && value > 0 && value <= MAX_CLOUD_RESOURCE_INT) {
    return value;
  }
  if (typeof value === "string" && /^\d+$/.test(value.trim())) {
    const parsed = Number.parseInt(value.trim(), 10);
    if (Number.isSafeInteger(parsed) && parsed > 0 && parsed <= MAX_CLOUD_RESOURCE_INT) {
      return parsed;
    }
  }
  throw new HttpError(400, "unsupported_cloud_resource_value", `${pointer} must be a positive integer`);
}

const MAX_CLOUD_RESOURCE_INT = 2_147_483_647;

function isRecord(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function packageToWire(artifact: PackageArtifactRecord) {
  return {
    package_digest: artifact.package_digest,
    upload_id: artifact.upload_id,
    byte_size: nullableNumber(artifact.byte_size),
    media_type: artifact.media_type,
    manifest_json: publicBoundaryJsonObject(artifact.manifest_json),
    resolved_experiment_json: publicBoundaryJsonObject(artifact.resolved_experiment_json),
    target: artifact.target,
    image_refs: artifact.image_refs.map(publicBoundaryText),
    secret_requirements: packageSecretRequirements(artifact),
    diagnostics: artifact.diagnostics.map(publicBoundaryImportDiagnostic),
    status: artifact.status,
    created_at: artifact.created_at,
    updated_at: artifact.updated_at,
  };
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
    });
  }
}

function packageSecretRequirements(artifact: PackageArtifactRecord): CloudSecretRequirement[] {
  const legacySecretFilesPointer = packageLegacySecretFilesPointer(artifact);
  if (legacySecretFilesPointer) {
    throw new HttpError(
      400,
      "invalid_package_secret_declaration",
      `${legacySecretFilesPointer} is not supported in Cloud packages; use /runtime/secrets`,
    );
  }
  const rawSecrets = jsonPointerValue(artifact.resolved_experiment_json, "/runtime/secrets");
  if (rawSecrets === undefined) {
    return [];
  }
  if (!Array.isArray(rawSecrets)) {
    throw new HttpError(400, "invalid_package_secret_declaration", "/runtime/secrets must be an array");
  }
  const requirements: CloudSecretRequirement[] = [];
  const seen = new Set<string>();
  for (const [idx, item] of rawSecrets.entries()) {
    const pointer = `/runtime/secrets/${idx}`;
    if (!isRecord(item)) {
      throw new HttpError(400, "invalid_package_secret_declaration", `${pointer} must be an object`);
    }
    const id = optionalPackageSecretString(item.name, `${pointer}/name`);
    const from = optionalPackageSecretString(item.from, `${pointer}/from`);
    if (from !== "env" && from !== "file") {
      throw new HttpError(
        400,
        "invalid_package_secret_declaration",
        `${pointer}/from '${from}' is not supported yet; supported providers are env and file`,
      );
    }
    if (id.includes("=")) {
      throw new HttpError(400, "invalid_package_secret_declaration", `${pointer}/name must not contain '='`);
    }
    if (!cloudSecretId(id)) {
      throw new HttpError(400, "invalid_package_secret_declaration", `${pointer}/name must be a Cloud secret id`);
    }
    if (seen.has(id)) {
      throw new HttpError(400, "invalid_package_secret_declaration", `Duplicate runtime secret declaration '${id}'`);
    }
    const mount = optionalPackageSecretMount(item.mount, `${pointer}/mount`);
    const target = packageSecretMountTarget(mount?.target, `${pointer}/mount/target`);
    seen.add(id);
    requirements.push({
      id,
      target,
      required_for_variants: secretRequiredForVariants(item, mount, pointer),
    });
  }
  return requirements.sort((left, right) => left.id.localeCompare(right.id));
}

function packageLegacySecretFilesPointer(artifact: PackageArtifactRecord): string | null {
  for (const pointer of [
    "/trial_runtime/agent/secret_files",
    "/agent/secret_files",
    "/stages/agent/secret_files",
  ]) {
    if (jsonPointerValue(artifact.resolved_experiment_json, pointer) !== undefined) {
      return pointer;
    }
  }
  return null;
}

function optionalPackageSecretString(value: unknown, pointer: string): string {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new HttpError(400, "invalid_package_secret_declaration", `${pointer} is required`);
  }
  return value.trim();
}

function optionalPackageSecretMount(value: unknown, pointer: string): JsonObject | null {
  if (value === undefined) {
    return null;
  }
  if (!isRecord(value)) {
    throw new HttpError(400, "invalid_package_secret_declaration", `${pointer} must be an object`);
  }
  return value;
}

function packageSecretMountTarget(value: unknown, pointer: string): string {
  if (value === undefined) {
    return "";
  }
  if (typeof value !== "string") {
    throw new HttpError(400, "invalid_package_secret_declaration", `${pointer} must be a string`);
  }
  const target = value.trim();
  if (target.length === 0) {
    return "";
  }
  if (
    !target.startsWith("/")
    || target.includes("\0")
    || target.includes("\n")
    || target.includes("\r")
    || target.split("/").some((part) => part === "..")
  ) {
    throw new HttpError(400, "invalid_package_secret_declaration", `${pointer} must be an absolute container path without parent traversal or control characters`);
  }
  return target;
}

function secretRequiredForVariants(secret: JsonObject, mount: JsonObject | null, pointer: string): string[] {
  if (mount && mount.required_for_variants !== undefined) {
    return packageSecretStringList(mount.required_for_variants, `${pointer}/mount/required_for_variants`);
  }
  if (secret.required_for_variants !== undefined) {
    return packageSecretStringList(secret.required_for_variants, `${pointer}/required_for_variants`);
  }
  return [];
}

function packageSecretStringList(value: unknown, pointer: string): string[] {
  if (!Array.isArray(value)) {
    throw new HttpError(400, "invalid_package_secret_declaration", `${pointer} must be an array`);
  }
  return [...new Set(value.map((item, idx) => {
    if (typeof item !== "string" || item.trim().length === 0) {
      throw new HttpError(400, "invalid_package_secret_declaration", `${pointer}/${idx} must be a non-empty string`);
    }
    return item.trim();
  }))].sort();
}

function cloudSecretId(id: string): boolean {
  return /^[A-Za-z0-9_.-]+$/.test(id);
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

function runToWire(run: CloudRunRecord) {
  const envKeys = Object.keys(run.env ?? {}).sort();
  const secretIds = Object.keys(run.secret_refs ?? {}).sort();
  return {
    run_id: run.run_id,
    package_digest: run.package_digest,
    run_label: run.run_label,
    status: run.status,
    env_keys: envKeys,
    secret_ids: secretIds,
    runtime_options: publicBoundaryJsonObject(run.runtime_options),
    run_requirements: runRequirementsToWire(run.run_requirements),
    created_at: run.created_at,
    updated_at: run.updated_at,
    started_at: run.started_at,
    completed_at: run.completed_at,
    error_message: run.error_message === null ? null : publicBoundaryText(run.error_message),
  };
}

function runRequirementsToWire(requirements: RunRequirements): RunRequirements {
  return {
    executor: requirements.executor,
    requires: requirements.requires.map(publicBoundaryText),
    image_refs: requirements.image_refs.map(publicBoundaryText),
    secret_ids: requirements.secret_ids.map(publicBoundaryText),
    network_perimeter: {
      default: requirements.network_perimeter.default,
      task_sandbox: requirements.network_perimeter.task_sandbox,
      agent: requirements.network_perimeter.agent,
      egress_hosts: requirements.network_perimeter.egress_hosts.map(publicBoundaryText),
    },
    sidecars: requirements.sidecars.map(publicBoundaryText),
    accelerators: requirements.accelerators.map(publicBoundaryText),
    arch: requirements.arch,
    cpu_count: requirements.cpu_count,
    memory_mb: requirements.memory_mb,
    disk_mb: requirements.disk_mb,
    isolation: requirements.isolation,
    timeout_ms: requirements.timeout_ms,
    max_parallel_trials: requirements.max_parallel_trials,
  };
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
    payload: publicBoundaryJsonObject(event.payload) ?? {},
    created_at: event.created_at,
  };
}

async function requireAttemptToken(
  request: Request,
  runs: RunRepository,
  input: { attemptId: string; runnerInstanceId?: string | null; packageDigest?: string | null },
): Promise<void> {
  await runs.verifyAttemptToken({
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
  return requireUuidString(
    decodePathParam(pathname.slice("/v1/worker/run-attempts/".length, -suffix.length), "/attempt_id"),
    "/attempt_id",
  );
}

function packageContentPath(pathname: string): boolean {
  return pathname.startsWith("/v1/packages/") && pathname.endsWith("/content");
}

function packagePath(pathname: string): boolean {
  return pathname.startsWith("/v1/packages/") && !pathname.endsWith("/content");
}

function packageDigestFromContentPath(pathname: string): string {
  return decodePathParam(pathname.slice("/v1/packages/".length, -"/content".length), "/package_digest");
}
