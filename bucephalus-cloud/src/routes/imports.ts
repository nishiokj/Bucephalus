import { join } from "node:path";
import { authOwnerKey, type AuthContext } from "../auth";
import { loadConfig } from "../config";
import { HttpError, jsonResponse, optionalString, readJsonObject, requireString } from "../http";
import { ImportJobRecord, ImportRepository, UploadRecord } from "../imports/repository";
import { inspectSealedPackageArchive, SealedPackageInspectionError } from "../imports/sealedPackage";
import { materializeStoredObject, putUploadObject } from "../objectStorage";
import { PackageRepository } from "../packages/repository";
import { sha256Digest } from "../primitives";

const DEFAULT_MAX_UPLOAD_BYTES = 512 * 1024 * 1024;
const SHA256_DIGEST_PATTERN = /^sha256:[0-9a-f]{64}$/;

export async function handleImportRoute(
  request: Request,
  url: URL,
  imports: ImportRepository,
  packages: PackageRepository,
  auth?: AuthContext | null,
): Promise<Response | null> {
  const ownerKey = authOwnerKey(auth);
  if (request.method === "POST" && url.pathname === "/v1/uploads") {
    return createUpload(request, imports, ownerKey);
  }

  if (request.method === "PUT" && uploadContentPath(url.pathname)) {
    return putUploadContent(request, url, imports, ownerKey);
  }

  if (request.method === "POST" && uploadCompletePath(url.pathname)) {
    return completeUpload(url, imports, ownerKey);
  }

  if (request.method === "POST" && url.pathname === "/v1/imports/sealed-package") {
    return importSealedPackage(request, imports, packages, ownerKey);
  }

  if (request.method === "GET" && url.pathname === "/v1/imports") {
    const jobs = await imports.listImportJobs({ limit: limitFromUrl(url), ownerKey });
    return jsonResponse({ imports: jobs.map(importJobToWire) });
  }

  if (request.method === "GET" && importPath(url.pathname)) {
    const importId = decodeURIComponent(url.pathname.slice("/v1/imports/".length));
    const job = await imports.getImportJob(importId, ownerKey);
    if (!job) {
      throw new HttpError(404, "import_not_found", "Import not found");
    }
    return jsonResponse(importJobToWire(job));
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

async function createUpload(request: Request, imports: ImportRepository, ownerKey?: string): Promise<Response> {
  const body = await readJsonObject(request);
  const byteSize = uploadByteSize(body.byte_size);
  if (byteSize !== null && byteSize > maxUploadBytes()) {
    throw new HttpError(413, "upload_too_large", "Upload byte_size exceeds the configured limit", {
      max_upload_bytes: maxUploadBytes(),
      byte_size: byteSize,
    });
  }
  const expectedDigest = optionalString(body.expected_digest, "/expected_digest");
  if (expectedDigest && !SHA256_DIGEST_PATTERN.test(expectedDigest)) {
    throw new HttpError(400, "invalid_upload_digest", "expected_digest must be sha256:<64 lowercase hex chars>");
  }
  const upload = await imports.createUpload({
    filename: requireString(body.filename, "/filename"),
    mediaType: optionalString(body.media_type, "/media_type") ?? "application/octet-stream",
    expectedDigest,
    byteSize,
    ownerKey,
  });
  return jsonResponse(uploadToWire(upload), { status: 201 });
}

async function putUploadContent(
  request: Request,
  url: URL,
  imports: ImportRepository,
  ownerKey?: string,
): Promise<Response> {
  const uploadId = uploadIdFromContentPath(url.pathname);
  const upload = await imports.getUpload(uploadId, ownerKey);
  if (!upload) {
    throw new HttpError(404, "upload_not_found", "Upload not found");
  }
  const bytes = await readBoundedUploadBody(request, persistedUploadByteSize(upload.byte_size));
  const storagePath = await putUploadObject(uploadId, bytes, upload.media_type);
  const updated = await imports.markUploaded({
    uploadId,
    contentDigest: sha256Digest(bytes),
    byteSize: bytes.byteLength,
    storagePath,
    ownerKey,
  });
  return jsonResponse(uploadToWire(updated));
}

async function readBoundedUploadBody(request: Request, expectedBytes: number | null): Promise<Uint8Array> {
  const maxBytes = maxUploadBytes();
  const contentLength = request.headers.get("content-length");
  if (contentLength !== null) {
    const normalizedContentLength = contentLength.trim();
    const declared = /^[0-9]+$/.test(normalizedContentLength)
      ? Number.parseInt(normalizedContentLength, 10)
      : NaN;
    if (!Number.isSafeInteger(declared) || declared < 0) {
      throw new HttpError(400, "invalid_content_length", "Invalid upload content-length");
    }
    if (declared > maxBytes) {
      throw new HttpError(413, "upload_too_large", "Upload exceeds the configured size limit", {
        max_upload_bytes: maxBytes,
        content_length: declared,
      });
    }
    if (expectedBytes !== null && declared !== expectedBytes) {
      throw new HttpError(409, "upload_size_mismatch", "Upload content-length does not match declared byte_size", {
        expected_byte_size: expectedBytes,
        content_length: declared,
      });
    }
  }

  if (!request.body) {
    if (expectedBytes === 0 || expectedBytes === null) {
      return new Uint8Array();
    }
    throw new HttpError(409, "upload_size_mismatch", "Upload body is empty but byte_size is non-zero", {
      expected_byte_size: expectedBytes,
      byte_size: 0,
    });
  }

  const chunks: Uint8Array[] = [];
  let total = 0;
  const reader = request.body.getReader();
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      const chunk = value instanceof Uint8Array ? value : new Uint8Array(value);
      total += chunk.byteLength;
      if (total > maxBytes) {
        throw new HttpError(413, "upload_too_large", "Upload exceeds the configured size limit", {
          max_upload_bytes: maxBytes,
          byte_size: total,
        });
      }
      if (expectedBytes !== null && total > expectedBytes) {
        throw new HttpError(409, "upload_size_mismatch", "Upload body exceeds declared byte_size", {
          expected_byte_size: expectedBytes,
          byte_size: total,
        });
      }
      chunks.push(chunk);
    }
  } finally {
    reader.releaseLock();
  }

  if (expectedBytes !== null && total !== expectedBytes) {
    throw new HttpError(409, "upload_size_mismatch", "Upload body does not match declared byte_size", {
      expected_byte_size: expectedBytes,
      byte_size: total,
    });
  }

  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

function uploadByteSize(value: unknown): number | null {
  if (value === undefined || value === null) {
    return null;
  }
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new HttpError(400, "invalid_upload_size", "byte_size must be a non-negative safe integer");
  }
  return value;
}

function persistedUploadByteSize(value: unknown): number | null {
  if (value === undefined || value === null) {
    return null;
  }
  if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) {
    return value;
  }
  if (typeof value === "string" && /^[0-9]+$/.test(value)) {
    const parsed = Number.parseInt(value, 10);
    if (Number.isSafeInteger(parsed)) {
      return parsed;
    }
  }
  throw new HttpError(500, "invalid_persisted_upload_size", "Persisted upload byte_size is invalid");
}

function maxUploadBytes(): number {
  const raw = process.env.BUCEPHALUS_CLOUD_MAX_UPLOAD_BYTES;
  if (!raw) {
    return DEFAULT_MAX_UPLOAD_BYTES;
  }
  const parsed = Number.parseInt(raw, 10);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : DEFAULT_MAX_UPLOAD_BYTES;
}

async function completeUpload(url: URL, imports: ImportRepository, ownerKey?: string): Promise<Response> {
  const uploadId = uploadIdFromCompletePath(url.pathname);
  const upload = await imports.completeUpload(uploadId, ownerKey);
  return jsonResponse(uploadToWire(upload));
}

async function importSealedPackage(
  request: Request,
  imports: ImportRepository,
  packages: PackageRepository,
  ownerKey?: string,
): Promise<Response> {
  const body = await readJsonObject(request);
  const uploadId = requireString(body.upload_id, "/upload_id");
  const upload = await imports.getUpload(uploadId, ownerKey);
  if (!upload) {
    throw new HttpError(404, "upload_not_found", "Upload not found");
  }
  if (upload.status !== "completed" || !upload.storage_path) {
    throw new HttpError(409, "upload_not_completed", "Upload must be completed before import");
  }

  const importId = await imports.createImportJob({
    uploadId,
    label: optionalString(body.label, "/label"),
    ownerKey,
  });
  try {
    const importWorkDir = join(loadConfig().dataDir, "imports", importId);
    const inspectionWorkDir = join(importWorkDir, "extracted");
    const archivePath = await materializeStoredObject(upload.storage_path, join(importWorkDir, "archive"), "package.blob");
    const inspection = await inspectSealedPackageArchive({
      archivePath,
      workDir: inspectionWorkDir,
    });
    await imports.updateImportInspection({
      importId,
      status: "accepted",
      packageDigest: inspection.packageDigest,
      manifestJson: inspection.manifestJson,
      resolvedExperimentJson: inspection.resolvedExperimentJson,
      diagnostics: inspection.diagnostics,
    });
    if (inspection.packageDigest) {
      await packages.upsertArtifact({
        packageDigest: inspection.packageDigest,
        uploadId,
        storagePath: upload.storage_path,
        byteSize: persistedUploadByteSize(upload.byte_size),
        mediaType: upload.media_type,
        manifestJson: inspection.manifestJson,
        resolvedExperimentJson: inspection.resolvedExperimentJson,
        target: inspection.target,
        imageRefs: inspection.imageRefs,
        diagnostics: inspection.diagnostics,
        ownerKey,
      });
    }
  } catch (error) {
    await imports.updateImportInspection({
      importId,
      status: "failed",
      errorMessage: error instanceof Error ? error.message : String(error),
      diagnostics: error instanceof SealedPackageInspectionError ? error.diagnostics : [],
    });
  }

  const job = await imports.getImportJob(importId, ownerKey);
  if (!job) {
    throw new HttpError(500, "import_missing_after_create", "Import missing after creation");
  }
  return jsonResponse(importJobToWire(job), { status: 201 });
}

function uploadToWire(upload: UploadRecord) {
  return {
    upload_id: upload.upload_id,
    filename: upload.filename,
    media_type: upload.media_type,
    expected_digest: upload.expected_digest,
    content_digest: upload.content_digest,
    byte_size: persistedUploadByteSize(upload.byte_size),
    status: upload.status,
    created_at: upload.created_at,
    uploaded_at: upload.uploaded_at,
    completed_at: upload.completed_at,
    error_message: upload.error_message,
  };
}

function importJobToWire(job: ImportJobRecord) {
  return {
    import_id: job.import_id,
    upload_id: job.upload_id,
    import_type: job.import_type,
    status: job.status,
    label: job.label,
    package_digest: job.package_digest,
    error_message: job.error_message,
    diagnostics: job.diagnostics ?? [],
    created_at: job.created_at,
    updated_at: job.updated_at,
  };
}

function uploadContentPath(pathname: string): boolean {
  return pathname.startsWith("/v1/uploads/") && pathname.endsWith("/content");
}

function uploadCompletePath(pathname: string): boolean {
  return pathname.startsWith("/v1/uploads/") && pathname.endsWith("/complete");
}

function importPath(pathname: string): boolean {
  return pathname.startsWith("/v1/imports/") && pathname !== "/v1/imports/sealed-package";
}

function uploadIdFromContentPath(pathname: string): string {
  return decodeURIComponent(pathname.slice("/v1/uploads/".length, -"/content".length));
}

function uploadIdFromCompletePath(pathname: string): string {
  return decodeURIComponent(pathname.slice("/v1/uploads/".length, -"/complete".length));
}
