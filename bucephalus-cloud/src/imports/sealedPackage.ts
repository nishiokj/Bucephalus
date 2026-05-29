import { mkdir, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import * as tar from "tar";
import type { JsonObject, JsonValue } from "../primitives";

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
  imageRefs: string[];
  diagnostics: ImportDiagnostic[];
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
  const imageRefs = await collectPackagedTaskImageRefs(input.workDir);

  return {
    packageDigest,
    manifestJson,
    resolvedExperimentJson,
    imageRefs,
    diagnostics: manifestDiagnostics,
  };
}

async function findFirstFile(root: string, filename: string): Promise<string | null> {
  const glob = new Bun.Glob(`**/${filename}`);
  for await (const rel of glob.scan({ cwd: root, absolute: false, onlyFiles: true })) {
    return join(root, rel);
  }
  return null;
}

async function collectPackagedTaskImageRefs(root: string): Promise<string[]> {
  const tasksPath = await findFirstFile(root, "tasks.jsonl");
  if (!tasksPath) {
    return [];
  }
  const refs = new Set<string>();
  const text = await readFile(tasksPath, "utf8");
  for (const [idx, rawLine] of text.split(/\r?\n/).entries()) {
    const line = rawLine.trim();
    if (line.length === 0) {
      continue;
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      throw inspectionError(`tasks.jsonl line ${idx + 1} is not valid JSON`, {
        severity: "error",
        code: "invalid_tasks_jsonl",
        pointer: `/tasks/${idx}`,
        message: `tasks.jsonl line ${idx + 1} must parse as JSON.`,
      });
    }
    if (!isJsonObject(parsed)) {
      continue;
    }
    for (const pointer of [
      "/runtime/container_image/image",
      "/resources/workspace/image",
    ]) {
      const value = valueAt(parsed, pointer);
      if (typeof value === "string" && value.trim().length > 0) {
        refs.add(value.trim());
      }
    }
  }
  return [...refs].sort();
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
      message: `${pointer} must be a JSON object in the documented package manifest shape.`,
    });
  }
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
