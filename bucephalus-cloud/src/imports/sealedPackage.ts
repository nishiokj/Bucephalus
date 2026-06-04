import { mkdir, readFile, readdir, rm, lstat, stat } from "node:fs/promises";
import { join, posix as pathPosix, resolve } from "node:path";
import * as tar from "tar";
import { canonicalJsonStringify, sha256Digest, type JsonObject, type JsonValue } from "../primitives";

const SHA256_DIGEST_PATTERN = /^sha256:[0-9a-f]{64}$/;
const DEFAULT_MAX_ARCHIVE_ENTRIES = 10_000;
const DEFAULT_MAX_EXPANDED_BYTES = 512 * 1024 * 1024;
const MAX_SMALL_JSON_BYTES = 8 * 1024 * 1024;
const PACKAGE_CHECKS_FILE = "package_checks.json";
const PACKAGE_CHECKS_SCHEMA_VERSION = "package_checks_v1";
const STAGING_MANIFEST_FILE = "staging_manifest.json";
const PACKAGE_BLOBS_DIR = "blobs";
const CAS_POINTER_SCHEMA = "bucephalus_cas_pointer_v1";
const RUNTIME_PAYLOAD_ROOTS = new Set([
  "tasks",
  "files",
  "agent_builds",
  PACKAGE_BLOBS_DIR,
  "runtime_assets",
  "host_grader_capabilities",
]);

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
  await preflightSealedPackageArchive(input.archivePath);
  await rm(input.workDir, { recursive: true, force: true });
  await mkdir(input.workDir, { recursive: true });
  await tar.x({
    file: input.archivePath,
    cwd: input.workDir,
    strip: 0,
  });

  const manifestPath = join(input.workDir, "manifest.json");
  if (!(await isRegularFile(manifestPath))) {
    throw inspectionError("sealed package archive does not contain manifest.json", {
      severity: "error",
      code: "missing_manifest",
      pointer: "/",
      message: "Archive must contain manifest.json at the package root.",
    });
  }

  const manifestJson = parseJsonObject(await readFile(manifestPath, "utf8"), "manifest.json", "/manifest");
  const manifestDiagnostics = validateSealedManifest(manifestJson);
  throwIfDiagnosticsContainErrors("manifest.json is not a supported sealed_run_package_v2 manifest", manifestDiagnostics);

  const verified = await verifySealedPackageIntegrity(input.workDir, manifestJson);
  const resolvedExperimentJson = verified.resolvedExperimentJson;
  const packageDigest = typeof manifestJson.package_digest === "string" ? manifestJson.package_digest : null;
  const imageRefs = await collectPackageImageRefs(input.workDir, resolvedExperimentJson);

  return {
    packageDigest,
    manifestJson,
    resolvedExperimentJson,
    imageRefs,
    diagnostics: manifestDiagnostics,
  };
}

async function preflightSealedPackageArchive(archivePath: string): Promise<void> {
  let entries = 0;
  let expandedBytes = 0;
  let violation: SealedPackageInspectionError | null = null;
  const seenFilePaths = new Set<string>();
  await tar.t({
    file: archivePath,
    onentry(entry) {
      if (violation) {
        return;
      }
      entries += 1;
      if (entries > maxArchiveEntries()) {
        violation = inspectionError("sealed package archive contains too many entries", {
          severity: "error",
          code: "archive_entry_limit_exceeded",
          pointer: "/",
          message: `Archive contains more than ${maxArchiveEntries()} entries.`,
        });
        return;
      }

      let entryPath;
      try {
        entryPath = safeArchivePath(entry.path);
      } catch (error) {
        violation = error instanceof SealedPackageInspectionError
          ? error
          : inspectionError(String(error), {
            severity: "error",
            code: "unsafe_archive_path",
            pointer: "/",
            message: "Archive entry path is unsafe.",
          });
        return;
      }
      const entryType = String(entry.type);
      if (!["File", "OldFile", "ContiguousFile", "Directory"].includes(entryType)) {
        violation = inspectionError(`sealed package archive contains unsupported entry type '${entryType}'`, {
          severity: "error",
          code: "unsupported_archive_entry_type",
          pointer: `/${escapeJsonPointer(entryPath)}`,
          message: `Archive entry '${entryPath}' must be a regular file or directory.`,
        });
        return;
      }
      if (entryType !== "Directory") {
        if (seenFilePaths.has(entryPath)) {
          violation = inspectionError(`sealed package archive contains duplicate file '${entryPath}'`, {
            severity: "error",
            code: "duplicate_archive_entry",
            pointer: `/${escapeJsonPointer(entryPath)}`,
            message: `Archive contains duplicate file '${entryPath}'.`,
          });
          return;
        }
        seenFilePaths.add(entryPath);
        expandedBytes += Number(entry.size ?? 0);
        if (expandedBytes > maxExpandedBytes()) {
          violation = inspectionError("sealed package archive expands beyond the configured byte limit", {
            severity: "error",
            code: "archive_expanded_size_limit_exceeded",
            pointer: "/",
            message: `Archive expands beyond ${maxExpandedBytes()} bytes.`,
          });
          return;
        }
      }
    },
  });
  if (violation) {
    throw violation;
  }
}

function safeArchivePath(rawPath: string): string {
  const path = rawPath.trim();
  if (
    path.length === 0
    || path.includes("\\")
    || path.startsWith("/")
    || path.split("/").some((part) => part === "..")
  ) {
    throw inspectionError(`sealed package archive contains unsafe entry path '${rawPath}'`, {
      severity: "error",
      code: "unsafe_archive_path",
      pointer: "/",
      message: "Archive entries must be non-empty relative paths without parent components.",
    });
  }
  return pathPosix.normalize(path);
}

async function findFirstFile(root: string, filename: string): Promise<string | null> {
  const glob = new Bun.Glob(`**/${filename}`);
  for await (const rel of glob.scan({ cwd: root, absolute: false, onlyFiles: true })) {
    return join(root, rel);
  }
  return null;
}

async function collectPackageImageRefs(root: string, resolvedExperimentJson: JsonObject): Promise<string[]> {
  const refs = new Set<string>();
  for (const ref of await collectPackagedTaskImageRefs(root)) {
    refs.add(ref);
  }
  for (const pointer of [
    "/trial_runtime/agent/image",
    "/trial_runtime/grader/separate/image",
    "/trial_runtime/task/workspace/image",
  ]) {
    const value = valueAt(resolvedExperimentJson, pointer);
    if (typeof value === "string" && value.trim().length > 0) {
      refs.add(value.trim());
    }
  }
  const sidecars = valueAt(resolvedExperimentJson, "/sidecars");
  if (isJsonObject(sidecars)) {
    for (const sidecar of Object.values(sidecars)) {
      if (!isJsonObject(sidecar)) {
        continue;
      }
      const image = sidecar.image;
      if (typeof image === "string" && image.trim().length > 0) {
        refs.add(image.trim());
      }
    }
  }
  return [...refs].sort();
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

async function verifySealedPackageIntegrity(
  packageDir: string,
  manifest: JsonObject,
): Promise<{ resolvedExperimentJson: JsonObject }> {
  const checksumsRef = requiredStringAt(manifest, "/checksums_ref", "sealed package manifest missing checksums_ref");
  validateMetadataRefOutsideRuntimePayload(checksumsRef, "checksums_ref");
  const checksumsPath = resolvePackagePathUnderRoot(packageDir, checksumsRef, "checksums_ref");
  const checksums = await parseSmallJsonObjectFile(checksumsPath, "checksums.json", "/checksums");
  if (valueAt(checksums, "/schema_version") !== "sealed_package_checksums_v2") {
    throw inspectionError("checksums schema_version must be sealed_package_checksums_v2", {
      severity: "error",
      code: "invalid_checksums_schema",
      pointer: "/checksums/schema_version",
      message: "checksums.json schema_version must be sealed_package_checksums_v2.",
    });
  }
  const files = valueAt(checksums, "/files");
  if (!isJsonObject(files)) {
    throw inspectionError("checksums.json missing object field 'files'", {
      severity: "error",
      code: "missing_checksums_files",
      pointer: "/checksums/files",
      message: "checksums.json must include a files object.",
    });
  }

  for (const [rel, expectedDigest] of Object.entries(files)) {
    if (typeof expectedDigest !== "string" || !SHA256_DIGEST_PATTERN.test(expectedDigest)) {
      throw inspectionError(`checksums entry '${rel}' must be a sha256 digest`, {
        severity: "error",
        code: "invalid_checksum_digest",
        pointer: `/checksums/files/${escapeJsonPointer(rel)}`,
        message: `checksums.files['${rel}'] must be a sha256 digest.`,
      });
    }
    const filePath = resolvePackagePathUnderRoot(packageDir, rel, "checksums.files");
    if (!(await isRegularFile(filePath))) {
      throw inspectionError(`checksummed file missing or not regular: ${rel}`, {
        severity: "error",
        code: "checksummed_file_missing",
        pointer: `/checksums/files/${escapeJsonPointer(rel)}`,
        message: `Checksummed file '${rel}' must exist as a regular file.`,
      });
    }
    const actualDigest = sha256Digest(await readFile(filePath));
    if (actualDigest.toLowerCase() !== expectedDigest.toLowerCase()) {
      throw inspectionError(`checksum mismatch for '${rel}'`, {
        severity: "error",
        code: "checksum_mismatch",
        pointer: `/checksums/files/${escapeJsonPointer(rel)}`,
        message: `Checksum mismatch for '${rel}'.`,
      });
    }
  }

  for (const requiredRel of ["resolved_experiment.json", STAGING_MANIFEST_FILE]) {
    if (!Object.prototype.hasOwnProperty.call(files, requiredRel)) {
      throw inspectionError(`checksums must include '${requiredRel}'`, {
        severity: "error",
        code: "missing_required_checksummed_file",
        pointer: `/checksums/files/${escapeJsonPointer(requiredRel)}`,
        message: `checksums.files must include '${requiredRel}'.`,
      });
    }
  }

  await verifyNoUnsealedPackagePayloadEntries(packageDir, manifest, files);
  await verifyPackageCasPointers(packageDir, files);

  const computedDigest = sha256Digest(canonicalJsonStringify(files));
  const manifestDigest = requiredStringAt(manifest, "/package_digest", "sealed package manifest missing package_digest");
  if (computedDigest !== manifestDigest) {
    throw inspectionError("package digest mismatch", {
      severity: "error",
      code: "package_digest_mismatch",
      pointer: "/manifest/package_digest",
      message: `Manifest package_digest does not match computed package digest ${computedDigest}.`,
    });
  }

  const lockPath = join(packageDir, "package.lock");
  const lock = await parseSmallJsonObjectFile(lockPath, "package.lock", "/package_lock");
  if (valueAt(lock, "/schema_version") !== "sealed_package_lock_v1") {
    throw inspectionError("package.lock schema_version must be sealed_package_lock_v1", {
      severity: "error",
      code: "invalid_package_lock_schema",
      pointer: "/package_lock/schema_version",
      message: "package.lock schema_version must be sealed_package_lock_v1.",
    });
  }
  if (valueAt(lock, "/package_digest") !== manifestDigest) {
    throw inspectionError("package.lock digest does not match manifest package_digest", {
      severity: "error",
      code: "package_lock_digest_mismatch",
      pointer: "/package_lock/package_digest",
      message: "package.lock package_digest must match manifest package_digest.",
    });
  }

  const packageChecksRef = valueAt(manifest, "/package_checks_ref");
  if (typeof packageChecksRef === "string") {
    validateMetadataRefOutsideRuntimePayload(packageChecksRef, "package_checks_ref");
    const packageChecks = await parseSmallJsonObjectFile(
      resolvePackagePathUnderRoot(packageDir, packageChecksRef, "package_checks_ref"),
      "package_checks.json",
      "/package_checks",
    );
    if (valueAt(packageChecks, "/schema_version") !== PACKAGE_CHECKS_SCHEMA_VERSION) {
      throw inspectionError(`package checks schema_version must be ${PACKAGE_CHECKS_SCHEMA_VERSION}`, {
        severity: "error",
        code: "invalid_package_checks_schema",
        pointer: "/package_checks/schema_version",
        message: `package_checks.json schema_version must be ${PACKAGE_CHECKS_SCHEMA_VERSION}.`,
      });
    }
  }

  const resolvedExperimentJson = await parseSmallJsonObjectFile(
    resolvePackagePathUnderRoot(packageDir, "resolved_experiment.json", "checksums.files"),
    "resolved_experiment.json",
    "/resolved_experiment",
  );
  await parseSmallJsonObjectFile(
    resolvePackagePathUnderRoot(packageDir, STAGING_MANIFEST_FILE, "checksums.files"),
    STAGING_MANIFEST_FILE,
    "/staging_manifest",
  );
  return { resolvedExperimentJson };
}

async function parseSmallJsonObjectFile(path: string, filename: string, pointer: string): Promise<JsonObject> {
  let metadata;
  try {
    metadata = await stat(path);
  } catch {
    throw inspectionError(`${filename} is missing or unreadable`, {
      severity: "error",
      code: "missing_json_file",
      pointer,
      message: `${filename} must exist and be readable.`,
    });
  }
  if (!metadata.isFile() || metadata.size > MAX_SMALL_JSON_BYTES) {
    throw inspectionError(`${filename} is not a supported JSON file`, {
      severity: "error",
      code: "unsupported_json_file",
      pointer,
      message: `${filename} must be a regular JSON file no larger than ${MAX_SMALL_JSON_BYTES} bytes.`,
    });
  }
  return parseJsonObject(await readFile(path, "utf8"), filename, pointer);
}

function requiredStringAt(root: JsonObject, pointer: string, message: string): string {
  const value = valueAt(root, pointer);
  if (typeof value !== "string" || value.trim().length === 0) {
    throw inspectionError(message, {
      severity: "error",
      code: "missing_or_invalid_string",
      pointer,
      message: `${pointer} must be a non-empty string.`,
    });
  }
  return value;
}

function validateMetadataRefOutsideRuntimePayload(raw: string, fieldName: string): void {
  const rel = normalizePackageRel(raw, fieldName);
  const first = rel.split("/")[0] ?? "";
  if (RUNTIME_PAYLOAD_ROOTS.has(first)) {
    throw inspectionError(`${fieldName} must not point inside runtime payload directory '${first}'`, {
      severity: "error",
      code: "metadata_ref_inside_payload",
      pointer: `/manifest/${fieldName}`,
      message: `${fieldName} must not point inside runtime payload directory '${first}'.`,
    });
  }
}

function resolvePackagePathUnderRoot(packageDir: string, relPath: string, fieldName: string): string {
  const rel = normalizePackageRel(relPath, fieldName);
  const root = resolve(packageDir);
  const resolved = resolve(root, rel);
  if (resolved !== root && !resolved.startsWith(`${root}/`)) {
    throw inspectionError(`${fieldName} escapes package root`, {
      severity: "error",
      code: "package_path_escape",
      pointer: "/",
      message: `${fieldName} must resolve inside the package root.`,
    });
  }
  return resolved;
}

function normalizePackageRel(raw: string, fieldName: string): string {
  const trimmed = raw.trim();
  if (
    trimmed.length === 0
    || trimmed.includes("\\")
    || trimmed.startsWith("/")
    || trimmed.split("/").some((part) => part === "..")
  ) {
    throw inspectionError(`${fieldName} must be a safe relative package path`, {
      severity: "error",
      code: "unsafe_package_path",
      pointer: "/",
      message: `${fieldName} must be a non-empty relative package path without parent components.`,
    });
  }
  return pathPosix.normalize(trimmed);
}

async function verifyNoUnsealedPackagePayloadEntries(
  packageDir: string,
  manifest: JsonObject,
  checksumFiles: JsonObject,
): Promise<void> {
  const metadataPaths = packageMetadataPaths(manifest);
  for (const rel of await listPackageEntries(packageDir)) {
    const path = join(packageDir, rel);
    const metadata = await lstat(path);
    if (metadata.isDirectory()) {
      continue;
    }
    if (metadata.isSymbolicLink()) {
      throw inspectionError(`sealed package contains unsealed symlink '${rel}'`, {
        severity: "error",
        code: "unsealed_symlink",
        pointer: `/${escapeJsonPointer(rel)}`,
        message: `Sealed package must not contain symlink '${rel}'.`,
      });
    }
    if (!metadata.isFile()) {
      throw inspectionError(`sealed package contains unsupported file type '${rel}'`, {
        severity: "error",
        code: "unsupported_package_entry_type",
        pointer: `/${escapeJsonPointer(rel)}`,
        message: `Sealed package entry '${rel}' must be a regular file.`,
      });
    }
    if (metadataPaths.has(rel) || Object.prototype.hasOwnProperty.call(checksumFiles, rel)) {
      continue;
    }
    throw inspectionError(`sealed package contains unchecksummed payload file '${rel}'`, {
      severity: "error",
      code: "unchecksummed_payload_file",
      pointer: `/${escapeJsonPointer(rel)}`,
      message: `Payload file '${rel}' must be listed in checksums.json.`,
    });
  }
}

function packageMetadataPaths(manifest: JsonObject): Set<string> {
  const paths = new Set(["manifest.json", "package.lock", "checksums.json", PACKAGE_CHECKS_FILE]);
  const checksumsRef = valueAt(manifest, "/checksums_ref");
  if (typeof checksumsRef === "string") {
    paths.add(normalizePackageRel(checksumsRef, "checksums_ref"));
  }
  const packageChecksRef = valueAt(manifest, "/package_checks_ref");
  if (typeof packageChecksRef === "string") {
    paths.add(normalizePackageRel(packageChecksRef, "package_checks_ref"));
  }
  return paths;
}

async function verifyPackageCasPointers(packageDir: string, checksumFiles: JsonObject): Promise<void> {
  for (const rel of await listPackageEntries(packageDir)) {
    const path = join(packageDir, rel);
    if (!(await isRegularFile(path))) {
      continue;
    }
    const pointer = await readCasPointer(path);
    if (!pointer) {
      continue;
    }
    const blobRel = packageBlobRelForDigest(pointer.digest);
    const blobPath = resolvePackagePathUnderRoot(packageDir, blobRel, "package CAS pointer");
    const metadata = await stat(blobPath).catch(() => null);
    if (!metadata?.isFile()) {
      throw inspectionError(`package CAS pointer '${rel}' references missing blob '${blobRel}'`, {
        severity: "error",
        code: "missing_package_cas_blob",
        pointer: `/${escapeJsonPointer(rel)}`,
        message: `Package CAS pointer '${rel}' references missing blob '${blobRel}'.`,
      });
    }
    if (metadata.size !== pointer.size_bytes) {
      throw inspectionError(`package CAS pointer '${rel}' blob size mismatch`, {
        severity: "error",
        code: "package_cas_blob_size_mismatch",
        pointer: `/${escapeJsonPointer(rel)}`,
        message: `Package CAS blob '${blobRel}' size does not match pointer metadata.`,
      });
    }
    const actualDigest = sha256Digest(await readFile(blobPath));
    const checksumDigest = checksumFiles[blobRel];
    if (actualDigest.toLowerCase() !== pointer.digest.toLowerCase()) {
      throw inspectionError(`package CAS pointer '${rel}' blob digest mismatch`, {
        severity: "error",
        code: "package_cas_blob_digest_mismatch",
        pointer: `/${escapeJsonPointer(rel)}`,
        message: `Package CAS blob '${blobRel}' digest does not match pointer metadata.`,
      });
    }
    if (typeof checksumDigest !== "string" || checksumDigest.toLowerCase() !== pointer.digest.toLowerCase()) {
      throw inspectionError(`package CAS blob '${blobRel}' checksum digest mismatch`, {
        severity: "error",
        code: "package_cas_checksum_mismatch",
        pointer: `/checksums/files/${escapeJsonPointer(blobRel)}`,
        message: `Package CAS blob '${blobRel}' must be checksummed with its pointer digest.`,
      });
    }
  }
}

async function readCasPointer(path: string): Promise<{ digest: string; size_bytes: number } | null> {
  const metadata = await stat(path);
  if (!metadata.isFile() || metadata.size > 4096) {
    return null;
  }
  const text = await readFile(path, "utf8");
  if (!text.startsWith("{")) {
    return null;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }
  if (
    !isJsonObject(parsed)
    || parsed.schema_version !== CAS_POINTER_SCHEMA
    || parsed.kind !== "file"
    || typeof parsed.digest !== "string"
    || !SHA256_DIGEST_PATTERN.test(parsed.digest)
    || typeof parsed.size_bytes !== "number"
    || !Number.isSafeInteger(parsed.size_bytes)
    || parsed.size_bytes < 0
  ) {
    return null;
  }
  return {
    digest: parsed.digest,
    size_bytes: parsed.size_bytes,
  };
}

function packageBlobRelForDigest(digest: string): string {
  if (!SHA256_DIGEST_PATTERN.test(digest)) {
    throw inspectionError("package CAS pointer has invalid digest", {
      severity: "error",
      code: "invalid_package_cas_digest",
      pointer: "/",
      message: "Package CAS pointer digest must be sha256:<64 hex chars>.",
    });
  }
  return `${PACKAGE_BLOBS_DIR}/sha256/${digest.slice("sha256:".length).toLowerCase()}/blob`;
}

async function listPackageEntries(packageDir: string): Promise<string[]> {
  const entries: string[] = [];
  async function visit(dir: string, relPrefix: string): Promise<void> {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const rel = relPrefix ? `${relPrefix}/${entry.name}` : entry.name;
      entries.push(rel);
      if (entry.isDirectory()) {
        await visit(join(dir, entry.name), rel);
      }
    }
  }
  await visit(packageDir, "");
  return entries.sort();
}

async function isRegularFile(path: string): Promise<boolean> {
  try {
    const metadata = await lstat(path);
    return metadata.isFile();
  } catch {
    return false;
  }
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
  const allowed = new Set([
    "schema_version",
    "created_at",
    "resolved_experiment",
    "checksums_ref",
    "package_checks_ref",
    "package_digest",
  ]);
  for (const key of Object.keys(manifest)) {
    if (!allowed.has(key)) {
      diagnostics.push({
        severity: "error",
        code: "unknown_manifest_key",
        pointer: `/manifest/${escapeJsonPointer(key)}`,
        message: `manifest.json contains unknown key '${key}'.`,
      });
    }
  }
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

function escapeJsonPointer(segment: string): string {
  return segment.replaceAll("~", "~0").replaceAll("/", "~1");
}

function positiveIntFromEnv(name: string, fallback: number): number {
  const raw = process.env[name];
  if (!raw) {
    return fallback;
  }
  const parsed = Number.parseInt(raw, 10);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : fallback;
}

function maxArchiveEntries(): number {
  return positiveIntFromEnv("BUCEPHALUS_CLOUD_IMPORT_MAX_ARCHIVE_ENTRIES", DEFAULT_MAX_ARCHIVE_ENTRIES);
}

function maxExpandedBytes(): number {
  return positiveIntFromEnv("BUCEPHALUS_CLOUD_IMPORT_MAX_EXPANDED_BYTES", DEFAULT_MAX_EXPANDED_BYTES);
}
