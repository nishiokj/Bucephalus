import type { Sql } from "../db/client";
import { HttpError } from "../http";
import type { CanonicalEntity, EntityKind, JsonObject } from "../primitives";
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

export interface ImportProposalRecord {
  proposal_id: string;
  import_id: string;
  kind: EntityKind;
  content_digest: string;
  schema_version: string;
  canonical_json: JsonObject;
  canonical_size_bytes: number;
  source_pointer: string;
  suggested_aliases: string[];
  status: string;
  created_at: string;
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
  proposed_entities: ImportProposalRecord[];
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
    status: "proposed" | "failed";
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

  async insertProposal(input: {
    importId: string;
    entity: CanonicalEntity;
    sourcePointer: string;
    suggestedAliases: string[];
  }): Promise<void> {
    await this.sql`
      insert into ingest.import_proposals (
        import_id,
        kind,
        content_digest,
        schema_version,
        canonical_json,
        canonical_size_bytes,
        source_pointer,
        suggested_aliases
      )
      values (
        ${input.importId},
        ${input.entity.kind},
        ${input.entity.contentDigest},
        ${input.entity.schemaVersion},
        ${this.sql.json(input.entity.canonicalJson)},
        ${input.entity.canonicalSizeBytes},
        ${input.sourcePointer},
        ${input.suggestedAliases}
      )
      on conflict (import_id, content_digest) do nothing
    `;
  }

  async getImportJob(importId: string): Promise<ImportJobRecord | null> {
    const jobs = await this.sql`
      select *
      from ingest.import_jobs
      where import_id = ${importId}
      limit 1
    `;
    const job = jobs[0] as Omit<ImportJobRecord, "proposed_entities"> | undefined;
    if (!job) {
      return null;
    }
    const proposals = await this.sql`
      select *
      from ingest.import_proposals
      where import_id = ${importId}
      order by created_at asc
    `;
    return {
      ...job,
      proposed_entities: proposals as unknown as ImportProposalRecord[],
    };
  }

  async getProposal(proposalId: string): Promise<ImportProposalRecord | null> {
    const rows = await this.sql`
      select *
      from ingest.import_proposals
      where proposal_id = ${proposalId}
      limit 1
    `;
    return (rows[0] as ImportProposalRecord | undefined) ?? null;
  }

  async markProposalStatus(proposalId: string, status: "registered" | "skipped"): Promise<void> {
    await this.sql`
      update ingest.import_proposals
      set status = ${status}
      where proposal_id = ${proposalId}
    `;
  }

  async recordActionResult(input: {
    importId: string;
    proposalId: string;
    action: string;
    status: string;
    message?: string | null;
  }): Promise<void> {
    await this.sql`
      insert into ingest.import_action_results (import_id, proposal_id, action, status, message)
      values (${input.importId}, ${input.proposalId}, ${input.action}, ${input.status}, ${input.message ?? null})
    `;
  }

  async markImportActionStatus(importId: string, status: "applied" | "partially_applied"): Promise<void> {
    await this.sql`
      update ingest.import_jobs
      set status = ${status},
          updated_at = now()
      where import_id = ${importId}
    `;
  }
}
