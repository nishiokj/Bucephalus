import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { loadConfig } from "../config";
import { HttpError, jsonResponse, optionalString, readJsonObject, requireString } from "../http";
import { ImportJobRecord, ImportRepository, UploadRecord } from "../imports/repository";
import { inspectSealedPackageArchive, SealedPackageInspectionError } from "../imports/sealedPackage";
import { sha256Digest, type CanonicalEntity } from "../primitives";
import { RegistryRepository } from "../registry/repository";

export async function handleImportRoute(
  request: Request,
  url: URL,
  imports: ImportRepository,
  registry: RegistryRepository,
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
    return importSealedPackage(request, imports);
  }

  if (request.method === "GET" && importPath(url.pathname)) {
    const importId = decodeURIComponent(url.pathname.slice("/v1/imports/".length));
    const job = await imports.getImportJob(importId);
    if (!job) {
      throw new HttpError(404, "import_not_found", "Import not found");
    }
    return jsonResponse(importJobToWire(job));
  }

  if (request.method === "POST" && importActionsPath(url.pathname)) {
    return applyImportActions(request, url, imports, registry);
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
      status: "proposed",
      packageDigest: inspection.packageDigest,
      manifestJson: inspection.manifestJson,
      resolvedExperimentJson: inspection.resolvedExperimentJson,
      diagnostics: inspection.diagnostics,
    });
    for (const proposal of inspection.proposals) {
      await imports.insertProposal({
        importId,
        entity: proposal.entity,
        sourcePointer: proposal.sourcePointer,
        suggestedAliases: proposal.suggestedAliases,
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

async function applyImportActions(
  request: Request,
  url: URL,
  imports: ImportRepository,
  registry: RegistryRepository,
): Promise<Response> {
  const importId = importIdFromActionsPath(url.pathname);
  const job = await imports.getImportJob(importId);
  if (!job) {
    throw new HttpError(404, "import_not_found", "Import not found");
  }
  const body = await readJsonObject(request);
  const actions = Array.isArray(body.actions) ? body.actions : [];
  const results = [];
  for (const rawAction of actions) {
    if (!isRecord(rawAction)) {
      continue;
    }
    const proposalId = requireString(rawAction.proposal_id, "/actions[]/proposal_id");
    const action = requireString(rawAction.action, "/actions[]/action");
    const proposal = await imports.getProposal(proposalId);
    if (!proposal || proposal.import_id !== importId) {
      results.push({ proposal_id: proposalId, action, status: "failed", message: "proposal not found" });
      continue;
    }
    try {
      if (action === "skip") {
        await imports.markProposalStatus(proposalId, "skipped");
      } else {
        const entity = proposalToEntity(proposal);
        await registry.register(entity);
        if (action === "create_alias" || action === "replace_alias") {
          const alias = optionalString(rawAction.alias, "/actions[]/alias") ?? proposal.suggested_aliases[0];
          if (!alias) {
            throw new HttpError(400, "alias_required", "Alias action requires an alias");
          }
          await registry.createAlias({
            kind: proposal.kind,
            alias,
            contentDigest: proposal.content_digest,
            scopeType: optionalString(rawAction.scope_type, "/actions[]/scope_type") ?? "global",
            scopeId: optionalString(rawAction.scope_id, "/actions[]/scope_id"),
            replaceExisting: action === "replace_alias",
          });
        }
        await imports.markProposalStatus(proposalId, "registered");
      }
      await imports.recordActionResult({ importId, proposalId, action, status: "applied" });
      results.push({
        proposal_id: proposalId,
        action,
        status: action === "skip" ? "skipped" : "applied",
        content_digest: proposal.content_digest,
        message: null,
      });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      await imports.recordActionResult({ importId, proposalId, action, status: "failed", message });
      results.push({ proposal_id: proposalId, action, status: "failed", content_digest: proposal.content_digest, message });
    }
  }
  const refreshedJob = await imports.getImportJob(importId);
  const hasFailedResult = results.some((result) => result.status === "failed");
  const importStatus = refreshedJob && !hasFailedResult && refreshedJob.proposed_entities.every((proposal) => proposal.status !== "proposed")
    ? "applied"
    : "partially_applied";
  await imports.markImportActionStatus(importId, importStatus);
  return jsonResponse({ import_status: importStatus, results });
}

function proposalToEntity(proposal: {
  kind: CanonicalEntity["kind"];
  schema_version: string;
  content_digest: string;
  canonical_json: CanonicalEntity["canonicalJson"];
  canonical_size_bytes: number;
}): CanonicalEntity {
  const canonicalBytes = new TextEncoder().encode(JSON.stringify(proposal.canonical_json));
  return {
    protocol: "bucephalus-canonical-json-v1",
    kind: proposal.kind,
    schemaVersion: proposal.schema_version,
    contentDigest: proposal.content_digest,
    canonicalJson: proposal.canonical_json,
    canonicalEnvelope: {
      canonical_json: proposal.canonical_json,
      kind: proposal.kind,
      protocol: "bucephalus-canonical-json-v1",
      schema_version: proposal.schema_version,
    },
    canonicalBytes,
    canonicalSizeBytes: proposal.canonical_size_bytes,
  };
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
    proposed_entities: job.proposed_entities.map((proposal) => ({
      proposal_id: proposal.proposal_id,
      kind: proposal.kind,
      content_digest: proposal.content_digest,
      schema_version: proposal.schema_version,
      canonical_json: proposal.canonical_json,
      canonical_size_bytes: proposal.canonical_size_bytes,
      source_pointer: proposal.source_pointer,
      suggested_aliases: proposal.suggested_aliases,
      status: proposal.status,
      created_at: proposal.created_at,
    })),
  };
}

function uploadContentPath(pathname: string): boolean {
  return pathname.startsWith("/v1/uploads/") && pathname.endsWith("/content");
}

function uploadCompletePath(pathname: string): boolean {
  return pathname.startsWith("/v1/uploads/") && pathname.endsWith("/complete");
}

function importActionsPath(pathname: string): boolean {
  return pathname.startsWith("/v1/imports/") && pathname.endsWith("/actions");
}

function importPath(pathname: string): boolean {
  return pathname.startsWith("/v1/imports/") && !pathname.endsWith("/actions") && pathname !== "/v1/imports/sealed-package";
}

function uploadIdFromContentPath(pathname: string): string {
  return decodeURIComponent(pathname.slice("/v1/uploads/".length, -"/content".length));
}

function uploadIdFromCompletePath(pathname: string): string {
  return decodeURIComponent(pathname.slice("/v1/uploads/".length, -"/complete".length));
}

function importIdFromActionsPath(pathname: string): string {
  return decodeURIComponent(pathname.slice("/v1/imports/".length, -"/actions".length));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
