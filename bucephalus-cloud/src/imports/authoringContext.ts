import { mkdir, readFile, rm, stat } from "node:fs/promises";
import { join, posix as pathPosix } from "node:path";
import * as tar from "tar";
import { parse as parseYaml } from "yaml";
import { HttpError } from "../http";
import { sha256Digest, type JsonObject } from "../primitives";

const DEFAULT_MAX_CONTEXT_ARCHIVE_ENTRIES = 10_000;
const DEFAULT_MAX_CONTEXT_EXPANDED_BYTES = 256 * 1024 * 1024;
const PROJECT_MANIFEST_PATHS = ["bucephalus.project.yaml", "bucephalus.project.yml"] as const;

export interface AuthoringContextInspection {
  entrypoint: string;
  projectManifest: AuthoringProjectManifestEvidence;
  expandedBytes: number;
  entries: number;
}

export interface AuthoringProjectManifestEvidence {
  schema_version: "bucephalus_project_v1";
  path: string;
  digest: string;
  project_id: string;
  package_source: string;
  source_root: string;
  entrypoint: string;
}

interface AuthoringContextArchiveInspection {
  expandedBytes: number;
  entries: number;
  files: string[];
}

interface ProjectManifestBoundary {
  evidence: AuthoringProjectManifestEvidence;
  include: string[];
  exclude: string[];
}

export async function extractAuthoringContextArchive(input: {
  archivePath: string;
  workDir: string;
  entrypoint: string;
  projectManifest?: AuthoringProjectManifestEvidence | null;
}): Promise<AuthoringContextInspection> {
  const entrypoint = safeAuthoringContextPath(input.entrypoint, "/entrypoint");
  const requestedProjectManifest = input.projectManifest
    ? validateProjectManifestEvidence(input.projectManifest, "/project_manifest")
    : null;
  if (requestedProjectManifest && requestedProjectManifest.entrypoint !== entrypoint) {
    throw new HttpError(400, "project_manifest_entrypoint_mismatch", "project_manifest.entrypoint must match entrypoint", {
      entrypoint,
      project_manifest_entrypoint: requestedProjectManifest.entrypoint,
    });
  }
  const archiveInspection = await preflightAuthoringContextArchive(input.archivePath);
  await rm(input.workDir, { recursive: true, force: true });
  await mkdir(input.workDir, { recursive: true });
  await tar.x({
    file: input.archivePath,
    cwd: input.workDir,
    strip: 0,
    filter(path) {
      validateAuthoringContextExtractionPath(path);
      return true;
    },
  });

  const entrypointStat = await stat(join(input.workDir, entrypoint)).catch(() => null);
  if (!entrypointStat?.isFile()) {
    throw new HttpError(400, "authoring_entrypoint_missing", "Authoring context entrypoint is missing from the uploaded archive", {
      entrypoint,
    });
  }
  const projectManifest = await verifyExtractedProjectManifest(input.workDir, requestedProjectManifest, entrypoint);
  enforceProjectManifestBoundary(projectManifest, archiveInspection.files);

  return {
    expandedBytes: archiveInspection.expandedBytes,
    entries: archiveInspection.entries,
    entrypoint,
    projectManifest: projectManifest.evidence,
  };
}

export function validateProjectManifestEvidence(raw: unknown, pointer: string): AuthoringProjectManifestEvidence {
  if (!isJsonObject(raw)) {
    throw new HttpError(400, "project_manifest_required", `${pointer} is required for authoring_context builds`);
  }
  const schemaVersion = requiredString(raw, "schema_version", `${pointer}/schema_version`);
  if (schemaVersion !== "bucephalus_project_v1") {
    throw new HttpError(400, "invalid_project_manifest", "project_manifest.schema_version must be bucephalus_project_v1", {
      schema_version: schemaVersion,
    });
  }
  const path = safeAuthoringContextPath(requiredString(raw, "path", `${pointer}/path`), `${pointer}/path`);
  const digest = requiredString(raw, "digest", `${pointer}/digest`);
  if (!/^sha256:[0-9a-f]{64}$/.test(digest)) {
    throw new HttpError(400, "invalid_project_manifest", "project_manifest.digest must be sha256:<64 lowercase hex chars>", {
      digest,
    });
  }
  const projectId = requiredString(raw, "project_id", `${pointer}/project_id`);
  const packageSource = requiredString(raw, "package_source", `${pointer}/package_source`);
  const sourceRoot = requiredString(raw, "source_root", `${pointer}/source_root`);
  const entrypoint = safeAuthoringContextPath(requiredString(raw, "entrypoint", `${pointer}/entrypoint`), `${pointer}/entrypoint`);
  return {
    schema_version: "bucephalus_project_v1",
    path,
    digest,
    project_id: projectId,
    package_source: packageSource,
    source_root: sourceRoot,
    entrypoint,
  };
}

async function verifyExtractedProjectManifest(
  workDir: string,
  expected: AuthoringProjectManifestEvidence | null,
  entrypoint: string,
): Promise<ProjectManifestBoundary> {
  const { manifestRel, bytes } = await readExtractedProjectManifest(workDir, expected);
  const digest = sha256Digest(bytes);
  if (expected && digest !== expected.digest) {
    throw new HttpError(409, "project_manifest_digest_mismatch", "Project manifest digest does not match uploaded build request", {
      path: manifestRel,
      expected_digest: expected.digest,
      content_digest: digest,
    });
  }
  let parsed: unknown;
  try {
    parsed = parseYaml(bytes.toString("utf8"));
  } catch (error) {
    throw new HttpError(400, "invalid_project_manifest", "Project manifest YAML is invalid", {
      path: manifestRel,
      cause: error instanceof Error ? error.message : String(error),
    });
  }
  if (!isJsonObject(parsed) || parsed.schema_version !== "bucephalus_project_v1") {
    throw new HttpError(400, "invalid_project_manifest", "Project manifest must declare schema_version bucephalus_project_v1", {
      path: manifestRel,
    });
  }
  const targets = parsed.targets;
  if (!isJsonObject(targets) || !isJsonObject(targets.hosted_cloud)) {
    throw new HttpError(400, "invalid_project_manifest", "Project manifest must declare targets.hosted_cloud", {
      path: manifestRel,
    });
  }
  const project = parsed.project;
  const projectId = isJsonObject(project) && typeof project.id === "string" && project.id.trim().length > 0
    ? project.id.trim()
    : null;
  if (!projectId) {
    throw new HttpError(400, "invalid_project_manifest", "Project manifest must declare project.id", {
      path: manifestRel,
    });
  }
  if (!/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(projectId)) {
    throw new HttpError(400, "invalid_project_manifest", "Project manifest project.id is invalid", {
      path: manifestRel,
      project_id: projectId,
    });
  }
  const packageSources = parsed.package_sources;
  if (!isJsonObject(packageSources)) {
    throw new HttpError(400, "invalid_project_manifest", "Project manifest must declare package_sources", {
      path: manifestRel,
    });
  }
  const matchingSources: Array<[string, JsonObject]> = [];
  for (const [name, rawSource] of Object.entries(packageSources)) {
    if (!isJsonObject(rawSource)) {
      throw new HttpError(400, "invalid_project_manifest", "Project manifest package source must be an object", {
        path: manifestRel,
        package_source: name,
      });
    }
    const entrypoints = stringArrayField(rawSource, "entrypoints", `package_sources.${name}.entrypoints`);
    if (entrypoints.includes(entrypoint)) {
      matchingSources.push([name, rawSource]);
    }
  }
  if (matchingSources.length !== 1) {
    throw new HttpError(400, "invalid_project_manifest", "Project manifest must declare the requested entrypoint in exactly one package source", {
      entrypoint,
      matches: matchingSources.map(([name]) => name),
    });
  }
  const [packageSource, rawSource] = matchingSources[0]!;
  if (expected && packageSource !== expected.package_source) {
    throw new HttpError(400, "invalid_project_manifest", "Project manifest does not declare the requested package source", {
      package_source: expected.package_source,
    });
  }
  const source = rawSource as JsonObject;
  const sourceRoot = validateManifestRelDir(optionalStringField(source, "root") ?? ".", `package_sources.${packageSource}.root`);
  if (!pathInSourceRoot(entrypoint, sourceRoot)) {
    throw new HttpError(400, "invalid_project_manifest", "Project manifest package source declares entrypoint outside its root", {
      package_source: packageSource,
      entrypoint,
      source_root: sourceRoot,
    });
  }
  if (expected && sourceRoot !== expected.source_root) {
    throw new HttpError(400, "invalid_project_manifest", "Project manifest package source does not declare the requested entrypoint", {
      package_source: expected.package_source,
      entrypoint: expected.entrypoint,
    });
  }
  const include = normalizeManifestPatterns(stringArrayField(source, "include", `package_sources.${packageSource}.include`), `package_sources.${packageSource}.include`);
  if (include.length === 0) {
    throw new HttpError(400, "invalid_project_manifest", "Project manifest package source include must not be empty", {
      package_source: packageSource,
    });
  }
  const exclude = normalizeManifestPatterns(optionalStringArrayField(source, "exclude", `package_sources.${packageSource}.exclude`), `package_sources.${packageSource}.exclude`);
  const evidence: AuthoringProjectManifestEvidence = {
    schema_version: "bucephalus_project_v1",
    path: manifestRel,
    digest,
    project_id: projectId,
    package_source: packageSource,
    source_root: sourceRoot,
    entrypoint,
  };
  if (expected && JSON.stringify(evidence) !== JSON.stringify(expected)) {
    throw new HttpError(400, "project_manifest_mismatch", "Project manifest evidence does not match uploaded build request", {
      expected,
      actual: evidence,
    });
  }
  return { evidence, include, exclude };
}

function enforceProjectManifestBoundary(boundary: ProjectManifestBoundary, files: string[]): void {
  const violations = files
    .filter((path) => !projectManifestIncludesFile(boundary, path))
    .slice(0, 20);
  if (violations.length > 0) {
    throw new HttpError(400, "authoring_context_outside_project_manifest", "Authoring context archive contains files outside the project manifest package source boundary", {
      package_source: boundary.evidence.package_source,
      manifest_path: boundary.evidence.path,
      violations,
    });
  }
}

function projectManifestIncludesFile(boundary: ProjectManifestBoundary, path: string): boolean {
  if (path === boundary.evidence.path || path === boundary.evidence.entrypoint) {
    return true;
  }
  if (boundary.exclude.some((pattern) => manifestPatternMatches(pattern, path))) {
    return false;
  }
  return boundary.include.some((pattern) => manifestPatternMatches(pattern, path));
}

async function readExtractedProjectManifest(
  workDir: string,
  expected: AuthoringProjectManifestEvidence | null,
): Promise<{ manifestRel: string; bytes: Buffer }> {
  if (expected) {
    const bytes = await readFile(join(workDir, expected.path)).catch((error) => {
      throw new HttpError(400, "project_manifest_missing", "Authoring context is missing the declared project manifest", {
        path: expected.path,
        cause: error instanceof Error ? error.message : String(error),
      });
    });
    return { manifestRel: expected.path, bytes };
  }

  const found: Array<{ manifestRel: string; bytes: Buffer }> = [];
  for (const manifestRel of PROJECT_MANIFEST_PATHS) {
    const bytes = await readFile(join(workDir, manifestRel)).catch(() => null);
    if (bytes) {
      found.push({ manifestRel, bytes });
    }
  }
  if (found.length === 1) {
    return found[0]!;
  }
  if (found.length > 1) {
    throw new HttpError(400, "project_manifest_ambiguous", "Authoring context must contain exactly one project manifest at the archive root", {
      paths: found.map((item) => item.manifestRel),
    });
  }
  throw new HttpError(400, "project_manifest_missing", "Authoring context is missing bucephalus.project.yaml or bucephalus.project.yml", {
    paths: [...PROJECT_MANIFEST_PATHS],
  });
}

export function safeAuthoringContextPath(rawPath: string, pointer: string): string {
  return safeAuthoringContextPathWithOptions(rawPath, pointer, false);
}

function safeAuthoringContextPathWithOptions(rawPath: string, pointer: string, allowDirectory: boolean): string {
  const trimmedPath = rawPath.trim();
  let path = trimmedPath;
  if (allowDirectory) {
    path = path.replace(/\/+$/g, "");
  }
  if (
    path.length === 0
    || trimmedPath !== rawPath
    || path.includes("\\")
    || path.startsWith("/")
    || path.split("/").some((part) => part.length === 0 || part === "." || part === "..")
  ) {
    throw new HttpError(400, "invalid_authoring_context_path", `${pointer} must be a relative POSIX path without empty, current, or parent components`);
  }
  return pathPosix.normalize(path);
}

function validateAuthoringContextEntry(rawPath: string, entryType: string): {
  path: string;
  isDirectory: boolean;
} {
  const isDirectory = entryType === "Directory";
  if (!["File", "OldFile", "ContiguousFile", "Directory"].includes(entryType)) {
    throw new HttpError(400, "unsupported_authoring_context_entry_type", "Authoring context archive entries must be regular files or directories", {
      path: rawPath,
      entry_type: entryType,
    });
  }
  const entryPath = safeAuthoringContextPathWithOptions(rawPath, "/archive", isDirectory);
  if (containsBlockedContextPath(entryPath, isDirectory)) {
    throw new HttpError(400, "blocked_authoring_context_path", "Authoring context archive contains a blocked path that should not be uploaded", {
      path: entryPath,
    });
  }
  return { path: entryPath, isDirectory };
}

function validateAuthoringContextExtractionPath(rawPath: string): string {
  const isDirectory = rawPath.endsWith("/");
  const path = safeAuthoringContextPathWithOptions(rawPath, "/archive", isDirectory);
  if (containsBlockedContextPath(path, isDirectory)) {
    throw new HttpError(400, "blocked_authoring_context_path", "Authoring context archive contains a blocked path that should not be uploaded", {
      path,
    });
  }
  return path;
}

async function preflightAuthoringContextArchive(archivePath: string): Promise<AuthoringContextArchiveInspection> {
  let entries = 0;
  let expandedBytes = 0;
  let violation: HttpError | null = null;
  const files: string[] = [];
  const seenPaths = new Set<string>();
  const seenFilePaths = new Set<string>();

  await tar.t({
    file: archivePath,
    onentry(entry) {
      if (violation) {
        return;
      }
      entries += 1;
      if (entries > maxContextArchiveEntries()) {
        violation = new HttpError(413, "authoring_context_entry_limit_exceeded", "Authoring context archive contains too many entries", {
          max_entries: maxContextArchiveEntries(),
        });
        return;
      }

      let validated;
      try {
        validated = validateAuthoringContextEntry(entry.path, String(entry.type));
      } catch (error) {
        violation = error instanceof HttpError
          ? error
          : new HttpError(400, "invalid_authoring_context_path", "Authoring context archive contains an unsafe path");
        return;
      }

      const entryPath = validated.path;
      if (seenPaths.has(entryPath)) {
        violation = new HttpError(400, "duplicate_authoring_context_entry", "Authoring context archive contains a duplicate path", {
          path: entryPath,
        });
        return;
      }
      if (hasDescendantPath(seenPaths, entryPath)) {
        violation = new HttpError(400, "conflicting_authoring_context_entry", "Authoring context archive contains conflicting file/directory paths", {
          path: entryPath,
        });
        return;
      }
      const parentFilePath = parentFileConflict(seenFilePaths, entryPath);
      if (parentFilePath) {
        violation = new HttpError(400, "conflicting_authoring_context_entry", "Authoring context archive contains an entry nested under a file path", {
          path: entryPath,
          parent_file_path: parentFilePath,
        });
        return;
      }
      seenPaths.add(entryPath);
      if (validated.isDirectory) {
        return;
      }
      seenFilePaths.add(entryPath);
      files.push(entryPath);
      expandedBytes += Number(entry.size ?? 0);
      if (expandedBytes > maxContextExpandedBytes()) {
        violation = new HttpError(413, "authoring_context_size_limit_exceeded", "Authoring context archive expands beyond the configured byte limit", {
          max_expanded_bytes: maxContextExpandedBytes(),
        });
      }
    },
  });

  if (violation) {
    throw violation;
  }
  return { entries, expandedBytes, files };
}

function parentFileConflict(seenFilePaths: Set<string>, path: string): string | null {
  const parts = path.split("/");
  for (let index = 1; index < parts.length; index += 1) {
    const parent = parts.slice(0, index).join("/");
    if (seenFilePaths.has(parent)) {
      return parent;
    }
  }
  return null;
}

function hasDescendantPath(seenPaths: Set<string>, path: string): boolean {
  const prefix = `${path}/`;
  for (const seen of seenPaths) {
    if (seen.startsWith(prefix)) {
      return true;
    }
  }
  return false;
}

function containsBlockedContextPath(path: string, isDirectory: boolean): boolean {
  const parts = path.split("/");
  return parts.some((part, index) => {
    const lower = part.toLowerCase();
    if (
      lower === ".git"
      || lower === ".env"
      || lower.startsWith(".env.")
      || lower === ".npmrc"
      || lower === ".pypirc"
      || lower === ".netrc"
      || lower === ".dockercfg"
      || lower === "id_rsa"
      || lower === "id_dsa"
      || lower === "id_ecdsa"
      || lower === "id_ed25519"
      || lower === "application_default_credentials.json"
    ) {
      return true;
    }
    if (
      lower === ".config"
      && index < parts.length - 1
      && parts[index + 1]?.toLowerCase() === "gcloud"
    ) {
      return true;
    }
    if (
      lower === "target"
      || lower === "node_modules"
      || lower === ".bucephalus"
      || lower === ".bucephalus-package"
      || lower === ".ssh"
      || lower === ".aws"
      || lower === ".azure"
      || lower === ".docker"
      || lower === ".gnupg"
      || lower === "gcloud"
    ) {
      return isDirectory || index < parts.length - 1;
    }
    return false;
  });
}

function isJsonObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function requiredString(object: JsonObject, key: string, pointer: string): string {
  const value = object[key];
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new HttpError(400, "invalid_project_manifest", `${pointer} must be a non-empty string`);
  }
  return value.trim();
}

function optionalStringField(object: JsonObject, key: string): string | null {
  const value = object[key];
  return typeof value === "string" && value.trim().length > 0 ? value.trim() : null;
}

function stringArrayField(object: JsonObject, key: string, pointer: string): string[] {
  const value = object[key];
  if (!Array.isArray(value)) {
    throw new HttpError(400, "invalid_project_manifest", `${pointer} must be a string array`);
  }
  return parseStringArray(value, pointer);
}

function optionalStringArrayField(object: JsonObject, key: string, pointer: string): string[] {
  const value = object[key];
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new HttpError(400, "invalid_project_manifest", `${pointer} must be a string array`);
  }
  return parseStringArray(value, pointer);
}

function parseStringArray(value: unknown[], pointer: string): string[] {
  return value.map((item, index) => {
    if (typeof item !== "string" || item.trim().length === 0) {
      throw new HttpError(400, "invalid_project_manifest", `${pointer}[${index}] must be a non-empty string`);
    }
    return item.trim();
  });
}

function validateManifestRelDir(raw: string, pointer: string): string {
  return validateManifestRelPath(raw, pointer);
}

function normalizeManifestPatterns(patterns: string[], pointer: string): string[] {
  return patterns.map((pattern) => validateManifestRelPath(pattern, pointer));
}

function validateManifestRelPath(raw: string, pointer: string): string {
  const trimmed = raw.trim();
  if (trimmed.length === 0 || trimmed.startsWith("/") || trimmed.includes("\\")) {
    throw new HttpError(400, "invalid_project_manifest", `${pointer} entries must be relative POSIX paths`);
  }
  if (trimmed.split("/").some((part) => part.length === 0 || part === "..")) {
    throw new HttpError(400, "invalid_project_manifest", `${pointer} entries cannot contain empty or parent path segments`);
  }
  return trimmed.replace(/\/+$/g, "");
}

function pathInSourceRoot(path: string, sourceRoot: string): boolean {
  return sourceRoot === "." || path === sourceRoot || path.startsWith(`${sourceRoot}/`);
}

function manifestPatternMatches(pattern: string, path: string): boolean {
  if (pattern === "**") {
    return true;
  }
  const prefix = pattern.endsWith("/**") ? pattern.slice(0, -3) : null;
  if (prefix) {
    return path === prefix || path.startsWith(`${prefix}/`);
  }
  return globSegmentsMatch(pattern.split("/"), path.split("/"));
}

function globSegmentsMatch(pattern: string[], path: string[]): boolean {
  if (pattern.length === 0) {
    return path.length === 0;
  }
  if (pattern[0] === "**") {
    return globSegmentsMatch(pattern.slice(1), path)
      || (path.length > 0 && globSegmentsMatch(pattern, path.slice(1)));
  }
  if (path.length === 0) {
    return false;
  }
  return globSegmentMatch(pattern[0]!, path[0]!) && globSegmentsMatch(pattern.slice(1), path.slice(1));
}

function globSegmentMatch(pattern: string, value: string): boolean {
  if (pattern === "*") {
    return true;
  }
  const parts = pattern.split("*");
  if (parts.length === 1) {
    return pattern === value;
  }
  let rest = value;
  for (const [index, part] of parts.entries()) {
    if (part.length === 0) {
      continue;
    }
    if (index === 0) {
      if (!rest.startsWith(part)) {
        return false;
      }
      rest = rest.slice(part.length);
      continue;
    }
    const found = rest.indexOf(part);
    if (found < 0) {
      return false;
    }
    rest = rest.slice(found + part.length);
  }
  return pattern.endsWith("*") || rest.length === 0;
}

function maxContextArchiveEntries(): number {
  return positiveIntegerEnv("BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_ENTRIES", DEFAULT_MAX_CONTEXT_ARCHIVE_ENTRIES);
}

function maxContextExpandedBytes(): number {
  return positiveIntegerEnv("BUCEPHALUS_CLOUD_MAX_AUTHORING_CONTEXT_EXPANDED_BYTES", DEFAULT_MAX_CONTEXT_EXPANDED_BYTES);
}

function positiveIntegerEnv(name: string, fallback: number): number {
  const parsed = Number.parseInt(process.env[name] ?? "", 10);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : fallback;
}
