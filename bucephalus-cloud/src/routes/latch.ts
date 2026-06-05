import { HttpError, isRecord, jsonResponse, optionalString, readJsonObject, requireRecord } from "../http";
import { RegistryRepository } from "../registry/repository";

const LATCH_MANIFEST_SCHEMA = "latch_manifest_v1";
const LATCH_RESOLUTION_SCHEMA = "latch_resolution_v1";
const SHA256_DIGEST_PATTERN = /^sha256:[0-9a-f]{64}$/;

export async function handleLatchRoute(
  request: Request,
  url: URL,
  registry: RegistryRepository,
): Promise<Response | null> {
  if (request.method === "POST" && url.pathname === "/v1/latch/resolve") {
    return resolveLatchBenchmark(request, registry);
  }
  return null;
}

async function resolveLatchBenchmark(request: Request, registry: RegistryRepository): Promise<Response> {
  const body = await readJsonObject(request);
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
  const caseLimit = positiveInteger(body.case_limit, "/case_limit")
    ?? positiveInteger(body.cases, "/cases")
    ?? undefined;
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
  const digest = optionalString(rawRef?.digest, "/benchmark_ref/digest");
  const alias = optionalString(rawRef?.alias, "/benchmark_ref/alias")
    ?? (rawBenchmark && !SHA256_DIGEST_PATTERN.test(rawBenchmark) ? rawBenchmark : null);
  const benchmarkDigest = digest ?? (rawBenchmark && SHA256_DIGEST_PATTERN.test(rawBenchmark) ? rawBenchmark : null);
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

function positiveInteger(value: unknown, pointer: string): number | null {
  if (value === undefined || value === null) {
    return null;
  }
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 1) {
    throw new HttpError(400, "invalid_request", `${pointer} must be a positive integer`);
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
