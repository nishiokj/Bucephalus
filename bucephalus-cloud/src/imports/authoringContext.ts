import { mkdir, rm, stat } from "node:fs/promises";
import { join, posix as pathPosix } from "node:path";
import * as tar from "tar";
import { HttpError } from "../http";

const DEFAULT_MAX_CONTEXT_ARCHIVE_ENTRIES = 10_000;
const DEFAULT_MAX_CONTEXT_EXPANDED_BYTES = 256 * 1024 * 1024;

export interface AuthoringContextInspection {
  entrypoint: string;
  expandedBytes: number;
  entries: number;
}

export async function extractAuthoringContextArchive(input: {
  archivePath: string;
  workDir: string;
  entrypoint: string;
}): Promise<AuthoringContextInspection> {
  const entrypoint = safeAuthoringContextPath(input.entrypoint, "/entrypoint");
  const inspection = await preflightAuthoringContextArchive(input.archivePath);
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

  return {
    ...inspection,
    entrypoint,
  };
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

async function preflightAuthoringContextArchive(archivePath: string): Promise<{
  entries: number;
  expandedBytes: number;
}> {
  let entries = 0;
  let expandedBytes = 0;
  let violation: HttpError | null = null;
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
  return { entries, expandedBytes };
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
