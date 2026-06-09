import { spawnSync } from "node:child_process";
import {
  canonicalizeEntity,
  normalizationHints,
  resolveRegistryRef,
  type EntityKind,
  type JsonObject,
  type RegistryResolverRepository,
} from "../primitives";

const SHA256_DIGEST_PATTERN = /^sha256:[0-9a-f]{64}$/;
const SCHEDULE_PREVIEW_ISSUE_CODES = new Set([
  "invalid_case_count",
  "invalid_max_concurrency",
  "invalid_matrix_repeats",
]);

export interface DraftDigestBinding {
  pointer: string;
  kind: EntityKind;
  contentDigest: string;
  resolution: "inline" | "inline_existing" | "inline_unregistered" | "alias" | "digest";
  alias?: string | null;
  displayName?: string | null;
}

export interface DraftIssue {
  severity: "error" | "warning" | "info";
  code: string;
  message: string;
  pointer?: string | null;
  relatedDigest?: string | null;
}

export interface DraftResolveResult {
  resolvedDraft: JsonObject;
  bindings: DraftDigestBinding[];
  unresolved: Array<{
    pointer: string;
    kind: EntityKind;
    reason: string;
  }>;
  issues: DraftIssue[];
}

export interface DraftValidationResult {
  valid: boolean;
  issues: DraftIssue[];
  resolvedRefs: DraftDigestBinding[];
  resolvedDraft: JsonObject;
}

export interface SchedulePreview {
  total_slots: number | null;
  variants: number;
  cases: number | null;
  repeats: number;
  seeds: number;
  max_concurrency: number | null;
  unresolved_refs: string[];
  warnings: DraftIssue[];
}

export async function canonicalizeDraft(draft: JsonObject): Promise<{
  canonicalDraft: JsonObject;
  draftDigest: string;
  digestMap: DraftDigestBinding[];
  issues: DraftIssue[];
}> {
  const canonical = canonicalizeEntity({
    kind: "experiment_package",
    schemaVersion: "experiment_yaml_v1_draft",
    object: draft,
  });
  const resolved = await resolveDraftRefs(draft, memoryRepository());
  return {
    canonicalDraft: canonical.canonicalJson,
    draftDigest: canonical.contentDigest,
    digestMap: resolved.bindings,
    issues: validationIssues(draft).concat(resolved.issues),
  };
}

export async function resolveDraftRefs(
  draft: JsonObject,
  repository: RegistryResolverRepository,
): Promise<DraftResolveResult> {
  const resolvedDraft = structuredClone(draft) as JsonObject;
  const bindings: DraftDigestBinding[] = [];
  const unresolved: DraftResolveResult["unresolved"] = [];
  const issues: DraftIssue[] = [];

  const variants = arrayAt(draft, "/matrix/variants");
  for (let index = 0; index < variants.length; index += 1) {
    const variant = variants[index];
    if (!isJsonObject(variant)) {
      continue;
    }
    const pointer = `/matrix/variants/${index}`;
    const ref = registryRefFromDraftEntity("variant", variant);
    if (ref) {
      try {
        validateDraftRegistryRef(ref);
        const resolved = await resolveRegistryRef(ref, repository, {
          registerInlineIfMissing: false,
          defaultSchemaVersion: "variant_v1",
        });
        bindings.push({
          pointer,
          kind: "variant",
          contentDigest: resolved.contentDigest,
          resolution: resolutionToBinding(resolved.resolution),
          alias: ref.alias ?? null,
          displayName: stringAt(variant, "/display_name") ?? stringAt(variant, "/name") ?? stringAt(variant, "/id"),
        });
        setDigestAnnotation(resolvedDraft, pointer, resolved.contentDigest);
        if (resolved.resolution === "inline_unregistered") {
          issues.push({
            severity: "info",
            code: "inline_variant_unregistered",
            message: "Inline variant has a stable digest but is not registered. Registering it is an explicit action.",
            pointer,
            relatedDigest: resolved.contentDigest,
          });
        }
      } catch (error) {
        unresolved.push({
          pointer,
          kind: "variant",
          reason: error instanceof Error ? error.message : String(error),
        });
      }
    } else {
      const canonical = canonicalizeEntity({
        kind: "variant",
        schemaVersion: "variant_v1",
        object: variant,
      });
      bindings.push({
        pointer,
        kind: "variant",
        contentDigest: canonical.contentDigest,
        resolution: "inline",
        displayName: stringAt(variant, "/display_name") ?? stringAt(variant, "/name") ?? stringAt(variant, "/id"),
      });
      setDigestAnnotation(resolvedDraft, pointer, canonical.contentDigest);
      issues.push(
        ...normalizationHints({ kind: "variant", object: variant }).suggestions.map((suggestion) => ({
          severity: suggestion.severity,
          code: suggestion.code,
          message: suggestion.message,
          pointer: suggestion.pointer ? `${pointer}${suggestion.pointer}` : pointer,
          relatedDigest: suggestion.relatedDigest ?? null,
        })),
      );
    }
  }

  const metrics = arrayAt(draft, "/metrics");
  for (let index = 0; index < metrics.length; index += 1) {
    const metric = metrics[index];
    if (!isJsonObject(metric)) {
      continue;
    }
    const pointer = `/metrics/${index}`;
    const canonical = canonicalizeEntity({
      kind: "metric",
      schemaVersion: "metric_v1",
      object: metric,
    });
    bindings.push({
      pointer,
      kind: "metric",
      contentDigest: canonical.contentDigest,
      resolution: "inline",
      displayName: stringAt(metric, "/display_name") ?? stringAt(metric, "/name") ?? stringAt(metric, "/id"),
    });
    setDigestAnnotation(resolvedDraft, pointer, canonical.contentDigest);
  }

  return {
    resolvedDraft,
    bindings,
    unresolved,
    issues,
  };
}

export async function validateDraft(
  draft: JsonObject,
  repository: RegistryResolverRepository = memoryRepository(),
): Promise<DraftValidationResult> {
  const resolved = await resolveDraftRefs(draft, repository);
  const issues = validationIssues(draft).concat(resolved.issues);
  for (const unresolvedRef of resolved.unresolved) {
    issues.push({
      severity: "warning",
      code: "unresolved_registry_ref",
      message: unresolvedRef.reason,
      pointer: unresolvedRef.pointer,
    });
  }
  return {
    valid: !issues.some((issue) => issue.severity === "error"),
    issues,
    resolvedRefs: resolved.bindings,
    resolvedDraft: resolved.resolvedDraft,
  };
}

export function previewSchedule(draft: JsonObject): SchedulePreview {
  const variants = arrayAt(draft, "/matrix/variants").length;
  const repeats = nonNegativeIntegerAt(draft, "/matrix/repeats") ?? 1;
  const seeds = arrayAt(draft, "/matrix/seeds").length || 1;
  const maxConcurrency = positiveIntegerAt(draft, "/scheduling/max_concurrency");
  const casesSource = stringAt(draft, "/matrix/cases/source");
  const casesCount = nonNegativeIntegerAt(draft, "/matrix/cases/count") ?? null;
  const cases = casesCount;
  const warnings = validationIssues(draft)
    .filter((issue) => issue.severity !== "error" || SCHEDULE_PREVIEW_ISSUE_CODES.has(issue.code));
  if (casesSource === "file" && casesCount === null && !hasValueAt(draft, "/matrix/cases/count")) {
    warnings.push({
      severity: "info",
      code: "case_count_unknown",
      message: "Case count is unknown because the draft references a local cases file. Core will count cases during build.",
      pointer: "/matrix/cases/path",
    });
  }

  return {
    total_slots: cases === null ? null : variants * cases * repeats,
    variants,
    cases,
    repeats,
    seeds,
    max_concurrency: maxConcurrency,
    unresolved_refs: [],
    warnings,
  };
}

export function exportDraftYaml(draft: JsonObject): string {
  const json = JSON.stringify(draft);
  const script = [
    "import json, sys, yaml",
    "data = json.loads(sys.stdin.read())",
    "sys.stdout.write(yaml.safe_dump(data, sort_keys=False))",
  ].join("\n");
  const result = spawnSync("python3", ["-c", script], {
    input: json,
    encoding: "utf8",
  });
  if (result.status !== 0) {
    throw new Error(result.stderr || "failed to export draft YAML");
  }
  return result.stdout;
}

function validationIssues(draft: JsonObject): DraftIssue[] {
  const issues: DraftIssue[] = [];
  const requiredObjects = [
    ["/experiment", "missing_experiment"],
    ["/runtime", "missing_runtime"],
    ["/matrix", "missing_matrix"],
    ["/stages", "missing_stages"],
    ["/policy", "missing_policy"],
  ] as const;
  for (const [pointer, code] of requiredObjects) {
    if (!isJsonObject(valueAt(draft, pointer))) {
      issues.push({
        severity: "error",
        code,
        message: `${pointer} must be present and must be an object`,
        pointer,
      });
    }
  }

  if (!stringAt(draft, "/experiment/id")) {
    issues.push({
      severity: "error",
      code: "missing_experiment_id",
      message: "/experiment/id is required",
      pointer: "/experiment/id",
    });
  }
  if (!stringAt(draft, "/experiment/name")) {
    issues.push({
      severity: "warning",
      code: "missing_experiment_name",
      message: "/experiment/name is recommended for authoring and display",
      pointer: "/experiment/name",
    });
  }
  if (!stringAt(draft, "/runtime/compute/backend")) {
    issues.push({
      severity: "error",
      code: "missing_compute_backend",
      message: "/runtime/compute/backend is required",
      pointer: "/runtime/compute/backend",
    });
  }
  if (arrayAt(draft, "/matrix/variants").length === 0) {
    issues.push({
      severity: "error",
      code: "missing_variants",
      message: "/matrix/variants must contain at least one variant",
      pointer: "/matrix/variants",
    });
  }
  if (!isJsonObject(valueAt(draft, "/matrix/cases"))) {
    issues.push({
      severity: "error",
      code: "missing_cases",
      message: "/matrix/cases is required",
      pointer: "/matrix/cases",
    });
  }
  const repeatsValue = valueAt(draft, "/matrix/repeats");
  if (repeatsValue === undefined || repeatsValue === null) {
    issues.push({
      severity: "warning",
      code: "default_repeats",
      message: "/matrix/repeats is absent; Core examples normally set it explicitly",
      pointer: "/matrix/repeats",
    });
  } else if (!isNonNegativeIntegerValue(repeatsValue)) {
    issues.push({
      severity: "error",
      code: "invalid_matrix_repeats",
      message: "/matrix/repeats must be a non-negative integer",
      pointer: "/matrix/repeats",
    });
  }
  if (hasValueAt(draft, "/matrix/cases/count") && !isNonNegativeIntegerValue(valueAt(draft, "/matrix/cases/count"))) {
    issues.push({
      severity: "error",
      code: "invalid_case_count",
      message: "/matrix/cases/count must be a non-negative integer",
      pointer: "/matrix/cases/count",
    });
  }
  if (hasValueAt(draft, "/scheduling/max_concurrency") && !isPositiveIntegerValue(valueAt(draft, "/scheduling/max_concurrency"))) {
    issues.push({
      severity: "error",
      code: "invalid_max_concurrency",
      message: "/scheduling/max_concurrency must be a positive integer",
      pointer: "/scheduling/max_concurrency",
    });
  }
  return issues;
}

function registryRefFromDraftEntity(kind: EntityKind, value: JsonObject) {
  const registry = value.registry;
  if (!isJsonObject(registry)) {
    return null;
  }
  const digest = typeof registry.digest === "string" ? registry.digest : null;
  const alias = typeof registry.alias === "string" ? registry.alias : null;
  const inline = isJsonObject(registry.inline) ? registry.inline : null;
  return Object.assign(
    {
      kind,
      schemaVersion: typeof registry.schema_version === "string" ? registry.schema_version : `${kind}_v1`,
      scopeType: typeof registry.scope_type === "string" ? registry.scope_type : "global",
      scopeId: typeof registry.scope_id === "string" ? registry.scope_id : null,
    },
    digest ? { digest } : {},
    alias ? { alias } : {},
    inline ? { inline } : {},
  );
}

function validateDraftRegistryRef(ref: ReturnType<typeof registryRefFromDraftEntity>): void {
  if (!ref?.digest) {
    return;
  }
  if (!SHA256_DIGEST_PATTERN.test(ref.digest)) {
    throw new Error("registry.digest must be sha256:<64 lowercase hex chars>");
  }
}

function resolutionToBinding(resolution: string): DraftDigestBinding["resolution"] {
  if (resolution === "digest" || resolution === "alias" || resolution === "inline_existing" || resolution === "inline_unregistered") {
    return resolution;
  }
  return "inline";
}

function setDigestAnnotation(root: JsonObject, pointer: string, digest: string): void {
  const value = valueAt(root, pointer);
  if (isJsonObject(value)) {
    const metadata = isJsonObject(value.__cloud) ? value.__cloud : {};
    metadata.digest = digest;
    value.__cloud = metadata;
  }
}

function memoryRepository(): RegistryResolverRepository {
  return {
    hasDigest: () => false,
    resolveAlias: () => null,
  };
}

function arrayAt(root: JsonObject, pointer: string): unknown[] {
  const value = valueAt(root, pointer);
  return Array.isArray(value) ? value : [];
}

function stringAt(root: JsonObject, pointer: string): string | null {
  const value = valueAt(root, pointer);
  return typeof value === "string" && value.length > 0 ? value : null;
}

function nonNegativeIntegerAt(root: JsonObject, pointer: string): number | null {
  const value = valueAt(root, pointer);
  return isNonNegativeIntegerValue(value) ? value : null;
}

function positiveIntegerAt(root: JsonObject, pointer: string): number | null {
  const value = valueAt(root, pointer);
  return isPositiveIntegerValue(value) ? value : null;
}

function isNonNegativeIntegerValue(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isPositiveIntegerValue(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}

function hasValueAt(root: JsonObject, pointer: string): boolean {
  const value = valueAt(root, pointer);
  return value !== undefined && value !== null;
}

function valueAt(root: JsonObject, pointer: string): unknown {
  if (pointer === "" || pointer === "/") {
    return root;
  }
  let current: unknown = root;
  for (const rawSegment of pointer.split("/").slice(1)) {
    const segment = rawSegment.replaceAll("~1", "/").replaceAll("~0", "~");
    if (Array.isArray(current)) {
      current = current[Number.parseInt(segment, 10)];
    } else if (isJsonObject(current)) {
      current = current[segment];
    } else {
      return undefined;
    }
  }
  return current;
}

function isJsonObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
