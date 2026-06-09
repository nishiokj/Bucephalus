import { authOwnerKey, type AuthContext } from "../auth";
import { decodePathParam, HttpError, isRecord, jsonResponse, optionalString, queryIntegerParam, readJsonObject, requireRecord, requireString } from "../http";
import type { LatchSubmissionRecord, LatchSubmissionRepository } from "../latch/repository";
import type { JsonObject } from "../primitives";
import { publicBoundaryJsonObject, publicBoundaryText } from "../publicBoundary";
import { RegistryRepository } from "../registry/repository";

const LATCH_MANIFEST_SCHEMA = "latch_manifest_v1";
const LATCH_RESOLUTION_SCHEMA = "latch_resolution_v1";
const SHA256_DIGEST_PATTERN = /^sha256:[0-9a-f]{64}$/;
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const LATCH_CASE_LIMIT_MAX = 200;

export async function handleLatchRoute(
  request: Request,
  url: URL,
  registry: RegistryRepository,
  submissions?: LatchSubmissionRepository,
  auth?: AuthContext | null,
): Promise<Response | null> {
  if (request.method === "POST" && url.pathname === "/v1/latch/resolve") {
    return resolveLatchBenchmark(request, registry);
  }
  if (request.method === "POST" && url.pathname === "/v1/latch/submissions") {
    if (!submissions) {
      throw new HttpError(500, "latch_submissions_unavailable", "Latch submissions repository is not configured");
    }
    return createLatchSubmission(request, submissions, authOwnerKey(auth));
  }
  if (request.method === "GET" && url.pathname === "/v1/latch/submissions") {
    if (!submissions) {
      throw new HttpError(500, "latch_submissions_unavailable", "Latch submissions repository is not configured");
    }
    const records = await submissions.listSubmissions({ limit: limitFromUrl(url), ownerKey: authOwnerKey(auth) });
    return jsonResponse({ submissions: records.map(submissionToWire) });
  }
  if (request.method === "GET" && url.pathname.startsWith("/v1/latch/submissions/")) {
    if (!submissions) {
      throw new HttpError(500, "latch_submissions_unavailable", "Latch submissions repository is not configured");
    }
    const submissionId = requireUuidString(
      decodePathParam(url.pathname.slice("/v1/latch/submissions/".length), "/submission_id"),
      "/submission_id",
    );
    const record = await submissions.getSubmission(submissionId, authOwnerKey(auth));
    if (!record) {
      throw new HttpError(404, "latch_submission_not_found", "Latch submission not found");
    }
    return jsonResponse(submissionToWire(record));
  }
  return null;
}

async function resolveLatchBenchmark(request: Request, registry: RegistryRepository): Promise<Response> {
  const body = await readJsonObject(request);
  const caseLimit = positiveInteger(body.case_limit, "/case_limit", { max: LATCH_CASE_LIMIT_MAX })
    ?? positiveInteger(body.cases, "/cases", { max: LATCH_CASE_LIMIT_MAX })
    ?? undefined;
  const benchmarkRef = benchmarkRefFromBody(body);
  const contentDigest = benchmarkRef.digest
    ?? await registry.resolveAlias("benchmark", benchmarkRef.alias, scopeInput(benchmarkRef));
  if (!contentDigest) {
    throw new HttpError(404, "benchmark_not_found", `Benchmark not found: ${benchmarkRef.label}`);
  }
  const object = await registry.getContentObject(contentDigest);
  if (!object || object.kind !== "benchmark") {
    throw new HttpError(404, "benchmark_not_found", `Benchmark not found: ${benchmarkRef.label}`);
  }
  const canonical = requireRecord(object.canonical_json, "/canonical_json");
  const tier = tierOneEligibility(canonical);
  if (!tier.eligible) {
    throw new HttpError(409, "benchmark_not_tier1_eligible", "Benchmark is not Tier-1 eligible", {
      benchmark: benchmarkRef.label,
      content_digest: contentDigest,
      reason: tier.reason,
      min_tier: tier.minTier,
    });
  }

  const manifest = latchManifestFromBenchmark(canonical);
  const limitedManifest = limitManifestCases(manifest, caseLimit);
  const materials = Array.isArray(canonical.materials) ? canonical.materials : [];

  return jsonResponse({
    schema_version: LATCH_RESOLUTION_SCHEMA,
    resolution_id: `latch_${Date.now()}`,
    benchmark: {
      id: benchmarkRef.label,
      content_digest: contentDigest,
      schema_version: object.schema_version,
      staging_shape: stringValue(canonical.staging_shape) ?? stringValue(recordValue(canonical.staging)?.shape) ?? "file",
      grader_shape: stringValue(canonical.grader_shape) ?? stringValue(recordValue(canonical.grading)?.shape) ?? "artifact_pure",
      tier_1_eligible: true,
    },
    manifest: limitedManifest,
    materials,
  });
}

function benchmarkRefFromBody(body: Record<string, unknown>): {
  label: string;
  alias: string;
  digest?: string;
  scopeType: string;
  scopeId?: string | null;
} {
  const rawRef = isRecord(body.benchmark_ref) ? body.benchmark_ref : null;
  const rawBenchmark = optionalString(body.benchmark, "/benchmark");
  const digest = optionalSha256(rawRef?.digest, "/benchmark_ref/digest");
  let alias = optionalString(rawRef?.alias, "/benchmark_ref/alias");
  let benchmarkDigest = digest;
  if (rawBenchmark) {
    if (SHA256_DIGEST_PATTERN.test(rawBenchmark)) {
      benchmarkDigest ??= rawBenchmark;
    } else if (rawBenchmark.toLowerCase().startsWith("sha256:")) {
      throw new HttpError(400, "invalid_digest", "/benchmark must be sha256:<64 lowercase hex chars>");
    } else {
      alias ??= rawBenchmark;
    }
  }
  if (!alias && !benchmarkDigest) {
    throw new HttpError(400, "benchmark_required", "Provide benchmark or benchmark_ref.alias/digest");
  }
  const result: {
    label: string;
    alias: string;
    digest?: string;
    scopeType: string;
    scopeId?: string | null;
  } = {
    label: alias ?? benchmarkDigest ?? "benchmark",
    alias: alias ?? "",
    scopeType: optionalString(rawRef?.scope_type, "/benchmark_ref/scope_type") ?? "global",
  };
  if (benchmarkDigest) {
    result.digest = benchmarkDigest;
  }
  const scopeId = optionalString(rawRef?.scope_id, "/benchmark_ref/scope_id");
  if (scopeId !== null) {
    result.scopeId = scopeId;
  }
  return result;
}

function latchManifestFromBenchmark(benchmark: Record<string, unknown>): Record<string, unknown> {
  const wrappedLatch = recordValue(benchmark.latch);
  const candidates = [
    benchmark,
    recordValue(benchmark.manifest),
    recordValue(benchmark.latch_manifest),
    recordValue(wrappedLatch?.manifest),
  ];
  const candidate = candidates.find((value): value is Record<string, unknown> =>
    value !== null && isLatchManifest(value)
  ) ?? null;
  if (!candidate) {
    throw new HttpError(400, "invalid_latch_benchmark", "Benchmark object must contain a latch_manifest_v1 manifest");
  }
  return structuredClone(candidate);
}

function isLatchManifest(value: unknown): boolean {
  return isRecord(value) && value.schema_version === LATCH_MANIFEST_SCHEMA && Array.isArray(value.cases);
}

function tierOneEligibility(benchmark: Record<string, unknown>): { eligible: boolean; reason: string; minTier: number } {
  if (benchmark.tier_1_eligible === false) {
    return { eligible: false, reason: "tier_1_eligible is false", minTier: positiveInteger(benchmark.min_tier, "/min_tier") ?? 2 };
  }
  const stagingShape = stringValue(benchmark.staging_shape) ?? stringValue(recordValue(benchmark.staging)?.shape);
  const graderShape = stringValue(benchmark.grader_shape) ?? stringValue(recordValue(benchmark.grading)?.shape);
  if (stagingShape && stagingShape !== "file") {
    return { eligible: false, reason: `staging_shape=${stagingShape}`, minTier: 2 };
  }
  if (graderShape && !["artifact_pure", "workspace_diff", "file", "text"].includes(graderShape)) {
    return { eligible: false, reason: `grader_shape=${graderShape}`, minTier: 2 };
  }
  return { eligible: true, reason: "eligible", minTier: 1 };
}

function limitManifestCases(manifest: Record<string, unknown>, caseLimit: number | undefined): Record<string, unknown> {
  if (!caseLimit) {
    return manifest;
  }
  const cases = Array.isArray(manifest.cases) ? manifest.cases : [];
  return {
    ...manifest,
    cases: cases.slice(0, caseLimit),
  };
}

function positiveInteger(value: unknown, pointer: string, bounds: { max?: number } = {}): number | null {
  if (value === undefined || value === null) {
    return null;
  }
  const max = bounds.max ?? Number.MAX_SAFE_INTEGER;
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 1 || value > max) {
    const rangeDescription = bounds.max === undefined ? "a positive integer" : `an integer from 1 to ${max}`;
    throw new HttpError(400, "invalid_request", `${pointer} must be ${rangeDescription}`, {
      pointer,
      min: 1,
      max,
    });
  }
  return value;
}

function stringValue(value: unknown): string | null {
  return typeof value === "string" && value.trim().length > 0 ? value : null;
}

function recordValue(value: unknown): Record<string, unknown> | null {
  return isRecord(value) ? value : null;
}

function scopeInput(input: { scopeType: string; scopeId?: string | null }): { scopeType: string; scopeId?: string | null } {
  return input.scopeId === undefined
    ? { scopeType: input.scopeType }
    : { scopeType: input.scopeType, scopeId: input.scopeId };
}

async function createLatchSubmission(
  request: Request,
  submissions: LatchSubmissionRepository,
  ownerKey?: string,
): Promise<Response> {
  const body = await readJsonObject(request);
  const benchmark = requireRecord(body.benchmark, "/benchmark");
  const resolution = requireRecord(body.resolution, "/resolution");
  const grading = recordValue(body.grading);
  const lifecycleGrading = recordValue(recordValue(body.lifecycle)?.grading);
  const archiveDigest = requireSha256(body.archive_digest, "/archive_digest");
  const record = await submissions.createSubmission({
    dispatchId: requireString(body.dispatch_id, "/dispatch_id"),
    uploadId: requireUuidString(body.upload_id, "/upload_id"),
    benchmarkRef: requireString(benchmark.id, "/benchmark/id"),
    benchmarkDigest: optionalSha256(benchmark.content_digest, "/benchmark/content_digest"),
    resolutionId: optionalString(resolution.resolution_id, "/resolution/resolution_id"),
    archiveDigest,
    gradingStatus: optionalString(grading?.status, "/grading/status")
      ?? optionalString(lifecycleGrading?.status, "/lifecycle/grading/status"),
    summaryJson: jsonObjectOrEmpty(body.summary, "/summary"),
    lifecycleJson: jsonObjectOrEmpty(body.lifecycle, "/lifecycle"),
    resultJson: jsonObjectOrEmpty(body.result, "/result"),
    ownerKey,
  });
  return jsonResponse(submissionToWire(record), { status: 201 });
}

function submissionToWire(record: LatchSubmissionRecord) {
  return {
    submission_id: record.submission_id,
    dispatch_id: publicBoundaryText(record.dispatch_id),
    upload_id: record.upload_id,
    benchmark: {
      id: publicBoundaryText(record.benchmark_ref),
      content_digest: record.benchmark_digest,
    },
    resolution_id: record.resolution_id === null ? null : publicBoundaryText(record.resolution_id),
    archive_digest: record.archive_digest,
    grading_status: record.grading_status === null ? null : publicBoundaryText(record.grading_status),
    summary: publicBoundaryJsonObject(record.summary_json),
    lifecycle: publicBoundaryJsonObject(record.lifecycle_json),
    result: publicBoundaryJsonObject(record.result_json),
    created_at: record.created_at,
    updated_at: record.updated_at,
  };
}

function jsonObjectOrEmpty(value: unknown, pointer: string): JsonObject {
  if (value === undefined || value === null) {
    return {};
  }
  if (!isRecord(value)) {
    throw new HttpError(400, "invalid_request", `${pointer} must be an object`);
  }
  return value as JsonObject;
}

function requireSha256(value: unknown, pointer: string): string {
  const digest = requireString(value, pointer);
  if (!SHA256_DIGEST_PATTERN.test(digest)) {
    throw new HttpError(400, "invalid_digest", `${pointer} must be sha256:<64 lowercase hex chars>`);
  }
  return digest;
}

function optionalSha256(value: unknown, pointer: string): string | null {
  const digest = optionalString(value, pointer);
  if (digest !== null && !SHA256_DIGEST_PATTERN.test(digest)) {
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

function limitFromUrl(url: URL): number {
  return queryIntegerParam(url, "limit", { defaultValue: 50, min: 1, max: 200 });
}
