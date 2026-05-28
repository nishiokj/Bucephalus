import { mkdir, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import * as tar from "tar";
import { canonicalizeEntity, type CanonicalEntity, type JsonObject, type JsonValue } from "../primitives";

const SHA256_DIGEST_PATTERN = /^sha256:[0-9a-f]{64}$/;

export interface ImportDiagnostic {
  severity: "info" | "warning" | "error";
  code: string;
  pointer: string;
  message: string;
}

export interface SealedPackageInspection {
  packageDigest: string | null;
  manifestJson: JsonObject;
  resolvedExperimentJson: JsonObject;
  diagnostics: ImportDiagnostic[];
  proposals: Array<{
    entity: CanonicalEntity;
    sourcePointer: string;
    suggestedAliases: string[];
  }>;
}

export class SealedPackageInspectionError extends Error {
  constructor(
    message: string,
    public readonly diagnostics: ImportDiagnostic[],
  ) {
    super(message);
    this.name = "SealedPackageInspectionError";
  }
}

export async function inspectSealedPackageArchive(input: {
  archivePath: string;
  workDir: string;
}): Promise<SealedPackageInspection> {
  await rm(input.workDir, { recursive: true, force: true });
  await mkdir(input.workDir, { recursive: true });
  await tar.x({
    file: input.archivePath,
    cwd: input.workDir,
    strip: 0,
  });

  const manifestPath = await findFirstFile(input.workDir, "manifest.json");
  if (!manifestPath) {
    throw inspectionError("sealed package archive does not contain manifest.json", {
      severity: "error",
      code: "missing_manifest",
      pointer: "/",
      message: "Archive does not contain manifest.json.",
    });
  }

  const manifestJson = parseJsonObject(await readFile(manifestPath, "utf8"), "manifest.json", "/manifest");
  const manifestDiagnostics = validateSealedManifest(manifestJson);
  throwIfDiagnosticsContainErrors("manifest.json is not a supported sealed_run_package_v2 manifest", manifestDiagnostics);

  const resolvedExperimentJson = manifestJson.resolved_experiment as JsonObject;
  const packageDigest = typeof manifestJson.package_digest === "string" ? manifestJson.package_digest : null;
  const { diagnostics: proposalDiagnostics, proposals } = proposeEntities(packageDigest, manifestJson, resolvedExperimentJson);
  throwIfDiagnosticsContainErrors("resolved_experiment does not match the documented current import shape", proposalDiagnostics);

  return {
    packageDigest,
    manifestJson,
    resolvedExperimentJson,
    diagnostics: [...manifestDiagnostics, ...proposalDiagnostics],
    proposals,
  };
}

function proposeEntities(
  packageDigest: string | null,
  manifestJson: JsonObject,
  resolved: JsonObject,
): Pick<SealedPackageInspection, "diagnostics" | "proposals"> {
  const diagnostics: ImportDiagnostic[] = [];
  const proposals: SealedPackageInspection["proposals"] = [];
  const experiment = objectAt(resolved, "/experiment");
  const experimentId = stringAt(resolved, "/experiment/id");
  const experimentName = stringAt(resolved, "/experiment/name");
  const variants = arrayAt(resolved, "/matrix/variants");
  const metrics = arrayAt(resolved, "/metrics");
  const runtime = objectAt(resolved, "/runtime");
  const cases = objectAt(resolved, "/matrix/cases");

  requireObjectForImport(diagnostics, resolved, "/experiment");
  requireObjectForImport(diagnostics, resolved, "/matrix");
  requireArrayForImport(diagnostics, resolved, "/matrix/variants");
  requireObjectForImport(diagnostics, resolved, "/matrix/cases");
  requireObjectForImport(diagnostics, resolved, "/runtime");
  requireArrayForImport(diagnostics, resolved, "/metrics");

  if (valueAt(resolved, "/matrix/tasks") !== undefined) {
    diagnostics.push({
      severity: "warning",
      code: "unsupported_legacy_or_alias_shape",
      pointer: "/matrix/tasks",
      message: "matrix.tasks is not imported; the current documented import shape uses matrix.cases.",
    });
  }
  if (valueAt(resolved, "/variant_plan") !== undefined) {
    diagnostics.push({
      severity: "warning",
      code: "unsupported_legacy_shape",
      pointer: "/variant_plan",
      message: "variant_plan is not imported; variants must be represented under matrix.variants.",
    });
  }
  if (valueAt(resolved, "/baseline") !== undefined) {
    diagnostics.push({
      severity: "warning",
      code: "unsupported_legacy_shape",
      pointer: "/baseline",
      message: "baseline is not imported as a legacy shape; baseline identity must be represented in matrix.variants.",
    });
  }

  for (const [index, variant] of variants.entries()) {
    if (!isJsonObject(variant)) {
      diagnostics.push({
        severity: "error",
        code: "invalid_variant",
        pointer: `/matrix/variants/${index}`,
        message: "Expected matrix.variants entries to be JSON objects.",
      });
    }
  }

  for (const [index, metric] of metrics.entries()) {
    if (!isJsonObject(metric)) {
      diagnostics.push({
        severity: "error",
        code: "invalid_metric",
        pointer: `/metrics/${index}`,
        message: "Expected metrics entries to be JSON objects.",
      });
    }
  }

  if (diagnostics.some((diagnostic) => diagnostic.severity === "error")) {
    return { diagnostics, proposals };
  }

  proposals.push({
    entity: canonicalizeEntity({
      kind: "experiment_package",
      schemaVersion: "experiment_package_v1",
      object: {
        experiment,
        package_digest: packageDigest,
        sealed_manifest: manifestJson,
      },
    }),
    sourcePointer: "/",
    suggestedAliases: [experimentId, experimentName].filter(isNonemptyString),
  });

  for (const [index, variant] of variants.entries()) {
    const variantObject = variant as JsonObject;
    proposals.push({
      entity: canonicalizeEntity({
        kind: "variant",
        schemaVersion: "variant_v1",
        object: variantObject,
      }),
      sourcePointer: `/matrix/variants/${index}`,
      suggestedAliases: [stringValue(variantObject.id), stringValue(variantObject.name), stringValue(variantObject.display_name)].filter(isNonemptyString),
    });
  }

  for (const [index, metric] of metrics.entries()) {
    const metricObject = metric as JsonObject;
    proposals.push({
      entity: canonicalizeEntity({
        kind: "metric",
        schemaVersion: "metric_v1",
        object: metricObject,
      }),
      sourcePointer: `/metrics/${index}`,
      suggestedAliases: [stringValue(metricObject.id), stringValue(metricObject.name), stringValue(metricObject.display_name)].filter(isNonemptyString),
    });
  }

  proposals.push({
    entity: canonicalizeEntity({
      kind: "runtime_profile",
      schemaVersion: "runtime_profile_v1",
      object: runtime as JsonObject,
    }),
    sourcePointer: "/runtime",
    suggestedAliases: [
      stringAt(resolved, "/runtime/compute/backend"),
      experimentId ? `${experimentId}-runtime` : null,
    ].filter(isNonemptyString),
  });

  proposals.push({
    entity: canonicalizeEntity({
      kind: "dataset",
      schemaVersion: "dataset_v1",
      object: cases as JsonObject,
    }),
    sourcePointer: "/matrix/cases",
    suggestedAliases: [
      stringValue((cases as JsonObject).suite_id),
      stringValue((cases as JsonObject).path),
      experimentId ? `${experimentId}-cases` : null,
    ].filter(isNonemptyString),
  });

  return { diagnostics, proposals };
}

async function findFirstFile(root: string, filename: string): Promise<string | null> {
  const glob = new Bun.Glob(`**/${filename}`);
  for await (const rel of glob.scan({ cwd: root, absolute: false, onlyFiles: true })) {
    return join(root, rel);
  }
  return null;
}

function parseJsonObject(text: string, filename: string, pointer: string): JsonObject {
  try {
    const parsed = JSON.parse(text) as unknown;
    if (isJsonObject(parsed)) {
      return parsed;
    }
  } catch {
    // Fall through to a structured inspection error below.
  }
  throw inspectionError(`${filename} is not a JSON object`, {
    severity: "error",
    code: "invalid_json_object",
    pointer,
    message: `${filename} must parse to a JSON object.`,
  });
}

function validateSealedManifest(manifest: JsonObject): ImportDiagnostic[] {
  const diagnostics: ImportDiagnostic[] = [];
  requireConst(diagnostics, manifest, "/schema_version", "sealed_run_package_v2");
  requireNonemptyString(diagnostics, manifest, "/created_at");
  requireObjectForImport(diagnostics, manifest, "/resolved_experiment");
  requireNonemptyString(diagnostics, manifest, "/checksums_ref");
  requireDigest(diagnostics, manifest, "/package_digest");
  if (valueAt(manifest, "/package_checks_ref") !== undefined) {
    requireNonemptyString(diagnostics, manifest, "/package_checks_ref");
  }
  return diagnostics;
}

function throwIfDiagnosticsContainErrors(message: string, diagnostics: ImportDiagnostic[]): void {
  if (diagnostics.some((diagnostic) => diagnostic.severity === "error")) {
    throw new SealedPackageInspectionError(message, diagnostics);
  }
}

function inspectionError(message: string, diagnostic: ImportDiagnostic): SealedPackageInspectionError {
  return new SealedPackageInspectionError(message, [diagnostic]);
}

function requireConst(
  diagnostics: ImportDiagnostic[],
  root: JsonObject,
  pointer: string,
  expected: string,
): void {
  if (valueAt(root, pointer) !== expected) {
    diagnostics.push({
      severity: "error",
      code: "invalid_const",
      pointer,
      message: `${pointer} must be ${expected}.`,
    });
  }
}

function requireNonemptyString(
  diagnostics: ImportDiagnostic[],
  root: JsonObject,
  pointer: string,
): void {
  const value = valueAt(root, pointer);
  if (typeof value !== "string" || value.trim().length === 0) {
    diagnostics.push({
      severity: "error",
      code: "missing_or_invalid_string",
      pointer,
      message: `${pointer} must be a non-empty string.`,
    });
  }
}

function requireDigest(
  diagnostics: ImportDiagnostic[],
  root: JsonObject,
  pointer: string,
): void {
  const value = valueAt(root, pointer);
  if (typeof value !== "string" || !SHA256_DIGEST_PATTERN.test(value)) {
    diagnostics.push({
      severity: "error",
      code: "missing_or_invalid_digest",
      pointer,
      message: `${pointer} must be a sha256 digest.`,
    });
  }
}

function requireObjectForImport(
  diagnostics: ImportDiagnostic[],
  root: JsonObject,
  pointer: string,
): void {
  if (!isJsonObject(valueAt(root, pointer))) {
    diagnostics.push({
      severity: "error",
      code: "missing_or_invalid_object",
      pointer,
      message: `${pointer} must be a JSON object in the documented import shape.`,
    });
  }
}

function requireArrayForImport(
  diagnostics: ImportDiagnostic[],
  root: JsonObject,
  pointer: string,
): void {
  if (!Array.isArray(valueAt(root, pointer))) {
    diagnostics.push({
      severity: "error",
      code: "missing_or_invalid_array",
      pointer,
      message: `${pointer} must be an array in the documented import shape.`,
    });
  }
}

function objectAt(root: JsonObject, pointer: string): JsonObject | null {
  const value = valueAt(root, pointer);
  return isJsonObject(value) ? value : null;
}

function arrayAt(root: JsonObject, pointer: string): unknown[] {
  const value = valueAt(root, pointer);
  return Array.isArray(value) ? value : [];
}

function stringAt(root: JsonObject, pointer: string): string | null {
  return stringValue(valueAt(root, pointer));
}

function valueAt(root: JsonObject, pointer: string): JsonValue | undefined {
  let current: unknown = root;
  for (const rawSegment of pointer.split("/").slice(1)) {
    const segment = rawSegment.replaceAll("~1", "/").replaceAll("~0", "~");
    if (!isJsonObject(current)) {
      return undefined;
    }
    current = current[segment];
  }
  return current as JsonValue | undefined;
}

function isJsonObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringValue(value: unknown): string | null {
  return typeof value === "string" && value.trim().length > 0 ? value : null;
}

function isNonemptyString(value: string | null): value is string {
  return typeof value === "string" && value.trim().length > 0;
}
