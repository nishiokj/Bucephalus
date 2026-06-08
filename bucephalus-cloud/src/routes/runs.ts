import { authOwnerKey, type AuthContext } from "../auth";
import { HttpError, jsonResponse, optionalString, readJsonObject, requireBearerToken, requireString } from "../http";
import { readStoredObject } from "../objectStorage";
import type { JsonObject, JsonValue } from "../primitives";
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
    const digest = packageDigestFromContentPath(url.pathname);
    await requireAttemptToken(request, runs, {
      attemptId: requireHeader(request, "x-bucephalus-attempt-id", "package content download"),
      packageDigest: digest,
    });
    const artifact = await packages.getArtifact(digest);
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
    const artifacts = await packages.listArtifacts({ limit, ownerKey });
    return jsonResponse({ packages: artifacts.map(packageToWire) });
  }

  if (request.method === "GET" && packagePath(url.pathname)) {
    const digest = decodeURIComponent(url.pathname.slice("/v1/packages/".length));
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
    const limit = limitFromUrl(url);
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
      return jsonResponse(await runtime.getSummary(runtimeRoute.runId));
    }
    if (runtimeRoute.kind === "events") {
      return jsonResponse({
        cloud_run_id: runtimeRoute.runId,
        events: await runtime.eventRows(runtimeRoute.runId, {
          limit: limitFromUrl(url),
          afterRowSeq: optionalIntFromUrl(url, "after_row_seq"),
        }),
      });
    }
    if (runtimeRoute.kind === "results") {
      return jsonResponse(await runtime.results(runtimeRoute.runId, {
        limit: limitFromUrl(url),
      }));
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

function optionalIntFromUrl(url: URL, key: string): number | undefined {
  const raw = url.searchParams.get(key);
  if (!raw) {
    return undefined;
  }
  const parsed = Number.parseInt(raw, 10);
  return Number.isFinite(parsed) ? parsed : undefined;
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
  const result = await runs.failAttempt({
    attemptId,
    runnerInstanceId,
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
  ownerKey?: string,
): Promise<Response> {
  const body = await readJsonObject(request);
  const packageDigest = requireString(body.package_digest, "/package_digest");
  const artifact = await packages.getArtifact(packageDigest, ownerKey);
  if (!artifact) {
    throw new HttpError(404, "package_not_found", "Package artifact not found");
  }
  if (artifact.status !== "accepted") {
    throw new HttpError(409, "package_not_runnable", "Package artifact is not accepted");
  }

  const secretRefs = cloudSecretRefs(requireStringMap(body.secret_refs, "/secret_refs"));
  const runtimeOptions = optionalJsonObject(body.runtime_options as JsonValue | undefined, "/runtime_options");
  validatePackageSecretRefs(artifact, secretRefs);

  const run = await runs.createRun({
    packageDigest,
    runLabel: optionalString(body.run_label, "/run_label"),
    env: requireStringMap(body.env, "/env"),
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

export function runRequirementsForArtifact(
  artifact: PackageArtifactRecord,
  runtimeOptions: JsonObject,
  secretRefs: Record<string, string> = {},
): RunRequirements {
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

function cloudNetworkPerimeter(
  artifact: PackageArtifactRecord,
  runtimeOptions: JsonObject,
): RunRequirements["network_perimeter"] {
  const runtimeNetwork = networkObject(runtimeOptions.network)
    ?? networkObject(packageRuntimeTopLevelValue(artifact, "network"));
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
  return ref.startsWith("gcp-secret-manager://") || ref.startsWith("aws-secrets-manager://");
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
    secret_requirements: packageSecretRequirements(artifact),
    diagnostics: artifact.diagnostics,
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
    runtime_options: run.runtime_options,
    run_requirements: run.run_requirements,
    created_at: run.created_at,
    updated_at: run.updated_at,
    started_at: run.started_at,
    completed_at: run.completed_at,
    error_message: run.error_message,
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
    payload: event.payload,
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
