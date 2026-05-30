import type { Sql } from "../db/client";
import { HttpError } from "../http";
import type { JsonObject } from "../primitives";
import type { ImportDiagnostic } from "./sealedPackage";

export interface UploadRecord {
  upload_id: string;
  filename: string;
  media_type: string;
  expected_digest: string | null;
  content_digest: string | null;
  byte_size: number | null;
  storage_path: string | null;
  status: string;
  created_at: string;
  uploaded_at: string | null;
  completed_at: string | null;
  error_message: string | null;
}

export interface ImportJobRecord {
  import_id: string;
  upload_id: string | null;
  import_type: string;
  status: string;
  label: string | null;
  package_digest: string | null;
  manifest_json: JsonObject | null;
  resolved_experiment_json: JsonObject | null;
  created_at: string;
  updated_at: string;
  error_message: string | null;
  diagnostics: ImportDiagnostic[];
}

export class ImportRepository {
  constructor(private readonly sql: Sql) {}

  async createUpload(input: {
    filename: string;
    mediaType: string;
    expectedDigest?: string | null;
    byteSize?: number | null;
  }): Promise<UploadRecord> {
    const rows = await this.sql`
      insert into ingest.uploads (filename, media_type, expected_digest, byte_size)
      values (${input.filename}, ${input.mediaType}, ${input.expectedDigest ?? null}, ${input.byteSize ?? null})
      returning *
    `;
    return rows[0] as UploadRecord;
  }

  async getUpload(uploadId: string): Promise<UploadRecord | null> {
    const rows = await this.sql`
      select *
      from ingest.uploads
      where upload_id = ${uploadId}
      limit 1
    `;
    return (rows[0] as UploadRecord | undefined) ?? null;
  }

  async markUploaded(input: {
    uploadId: string;
    contentDigest: string;
    byteSize: number;
    storagePath: string;
  }): Promise<UploadRecord> {
    const rows = await this.sql`
      update ingest.uploads
      set
        content_digest = ${input.contentDigest},
        byte_size = ${input.byteSize},
        storage_path = ${input.storagePath},
        status = 'uploaded',
        uploaded_at = now(),
        error_message = null
      where upload_id = ${input.uploadId}
      returning *
    `;
    const row = rows[0] as UploadRecord | undefined;
    if (!row) {
      throw new HttpError(404, "upload_not_found", "Upload not found");
    }
    return row;
  }

  async completeUpload(uploadId: string): Promise<UploadRecord> {
    const upload = await this.getUpload(uploadId);
    if (!upload) {
      throw new HttpError(404, "upload_not_found", "Upload not found");
    }
    if (!upload.content_digest) {
      throw new HttpError(409, "upload_missing_content", "Upload content has not been received");
    }
    if (upload.expected_digest && upload.expected_digest !== upload.content_digest) {
      await this.sql`
        update ingest.uploads
        set status = 'rejected',
            error_message = ${`expected ${upload.expected_digest}, got ${upload.content_digest}`}
        where upload_id = ${uploadId}
      `;
      throw new HttpError(409, "upload_digest_mismatch", "Upload digest does not match expected digest", {
        expected_digest: upload.expected_digest,
        content_digest: upload.content_digest,
      });
    }
    const rows = await this.sql`
      update ingest.uploads
      set status = 'completed',
          completed_at = now(),
          error_message = null
      where upload_id = ${uploadId}
      returning *
    `;
    return rows[0] as UploadRecord;
  }

  async createImportJob(input: {
    uploadId: string;
    label?: string | null;
    packageDigest?: string | null;
    manifestJson?: JsonObject | null;
    resolvedExperimentJson?: JsonObject | null;
  }): Promise<string> {
    const rows = await this.sql`
      insert into ingest.import_jobs (
        upload_id,
        import_type,
        status,
        label,
        package_digest,
        manifest_json,
        resolved_experiment_json
      )
      values (
        ${input.uploadId},
        'sealed_package',
        'inspecting',
        ${input.label ?? null},
        ${input.packageDigest ?? null},
        ${input.manifestJson ? this.sql.json(input.manifestJson) : null},
        ${input.resolvedExperimentJson ? this.sql.json(input.resolvedExperimentJson) : null}
      )
      returning import_id
    `;
    return (rows[0] as { import_id: string }).import_id;
  }

  async updateImportInspection(input: {
    importId: string;
    status: "accepted" | "failed";
    packageDigest?: string | null;
    manifestJson?: JsonObject | null;
    resolvedExperimentJson?: JsonObject | null;
    diagnostics?: ImportDiagnostic[];
    errorMessage?: string | null;
  }): Promise<void> {
    await this.sql`
      update ingest.import_jobs
      set status = ${input.status},
          package_digest = ${input.packageDigest ?? null},
          manifest_json = ${input.manifestJson ? this.sql.json(input.manifestJson) : null},
          resolved_experiment_json = ${input.resolvedExperimentJson ? this.sql.json(input.resolvedExperimentJson) : null},
          diagnostics = ${this.sql.json((input.diagnostics ?? []) as unknown as JsonObject)},
          error_message = ${input.errorMessage ?? null},
          updated_at = now()
      where import_id = ${input.importId}
    `;
  }

  async getImportJob(importId: string): Promise<ImportJobRecord | null> {
    const jobs = await this.sql`
      select *
      from ingest.import_jobs
      where import_id = ${importId}
      limit 1
    `;
    const job = jobs[0] as ImportJobRecord | undefined;
    if (!job) {
      return null;
    }
    return job;
  }
}
