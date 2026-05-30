import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { loadConfig } from "../config";
import { HttpError, jsonResponse, optionalString, readJsonObject, requireString } from "../http";
import { ImportJobRecord, ImportRepository, UploadRecord } from "../imports/repository";
import { inspectSealedPackageArchive, SealedPackageInspectionError } from "../imports/sealedPackage";
import { PackageRepository } from "../packages/repository";
import { sha256Digest } from "../primitives";

export async function handleImportRoute(
  request: Request,
  url: URL,
  imports: ImportRepository,
  packages: PackageRepository,
): Promise<Response | null> {
  if (request.method === "POST" && url.pathname === "/v1/uploads") {
    return createUpload(request, imports);
  }

  if (request.method === "PUT" && uploadContentPath(url.pathname)) {
    return putUploadContent(request, url, imports);
  }

  if (request.method === "POST" && uploadCompletePath(url.pathname)) {
    return completeUpload(url, imports);
  }

  if (request.method === "POST" && url.pathname === "/v1/imports/sealed-package") {
    return importSealedPackage(request, imports, packages);
  }

  if (request.method === "GET" && importPath(url.pathname)) {
    const importId = decodeURIComponent(url.pathname.slice("/v1/imports/".length));
    const job = await imports.getImportJob(importId);
    if (!job) {
      throw new HttpError(404, "import_not_found", "Import not found");
    }
    return jsonResponse(importJobToWire(job));
  }

  return null;
}

async function createUpload(request: Request, imports: ImportRepository): Promise<Response> {
  const body = await readJsonObject(request);
  const upload = await imports.createUpload({
    filename: requireString(body.filename, "/filename"),
    mediaType: optionalString(body.media_type, "/media_type") ?? "application/octet-stream",
    expectedDigest: optionalString(body.expected_digest, "/expected_digest"),
    byteSize: typeof body.byte_size === "number" ? body.byte_size : null,
  });
  return jsonResponse(uploadToWire(upload), { status: 201 });
}

async function putUploadContent(
  request: Request,
  url: URL,
  imports: ImportRepository,
): Promise<Response> {
  const uploadId = uploadIdFromContentPath(url.pathname);
  const upload = await imports.getUpload(uploadId);
  if (!upload) {
    throw new HttpError(404, "upload_not_found", "Upload not found");
  }
  const bytes = new Uint8Array(await request.arrayBuffer());
  const dataDir = loadConfig().dataDir;
  const uploadDir = join(dataDir, "uploads", uploadId);
  await mkdir(uploadDir, { recursive: true });
  const storagePath = join(uploadDir, upload.filename.replaceAll("/", "_"));
  await writeFile(storagePath, bytes);
  const updated = await imports.markUploaded({
    uploadId,
    contentDigest: sha256Digest(bytes),
    byteSize: bytes.byteLength,
    storagePath,
  });
  return jsonResponse(uploadToWire(updated));
}

async function completeUpload(url: URL, imports: ImportRepository): Promise<Response> {
  const uploadId = uploadIdFromCompletePath(url.pathname);
  const upload = await imports.completeUpload(uploadId);
  return jsonResponse(uploadToWire(upload));
}

async function importSealedPackage(
  request: Request,
  imports: ImportRepository,
  packages: PackageRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const uploadId = requireString(body.upload_id, "/upload_id");
  const upload = await imports.getUpload(uploadId);
  if (!upload) {
    throw new HttpError(404, "upload_not_found", "Upload not found");
  }
  if (upload.status !== "completed" || !upload.storage_path) {
    throw new HttpError(409, "upload_not_completed", "Upload must be completed before import");
  }

  const importId = await imports.createImportJob({
    uploadId,
    label: optionalString(body.label, "/label"),
  });
  try {
    const inspection = await inspectSealedPackageArchive({
      archivePath: upload.storage_path,
      workDir: join(loadConfig().dataDir, "imports", importId, "extracted"),
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
        byteSize: upload.byte_size,
        mediaType: upload.media_type,
        manifestJson: inspection.manifestJson,
        resolvedExperimentJson: inspection.resolvedExperimentJson,
        imageRefs: inspection.imageRefs,
        diagnostics: inspection.diagnostics,
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

  const job = await imports.getImportJob(importId);
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
    byte_size: upload.byte_size,
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
