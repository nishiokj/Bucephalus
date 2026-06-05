import type { Sql } from "../db/client";
import { HttpError } from "../http";
import type { JsonObject } from "../primitives";

export interface LatchSubmissionRecord {
  submission_id: string;
  dispatch_id: string;
  upload_id: string;
  owner_key: string | null;
  benchmark_ref: string;
  benchmark_digest: string | null;
  resolution_id: string | null;
  archive_digest: string;
  grading_status: string | null;
  summary_json: JsonObject;
  lifecycle_json: JsonObject;
  result_json: JsonObject;
  created_at: string;
  updated_at: string;
}

export class LatchSubmissionRepository {
  constructor(private readonly sql: Sql) {}

  async createSubmission(input: {
    dispatchId: string;
    uploadId: string;
    benchmarkRef: string;
    benchmarkDigest?: string | null;
    resolutionId?: string | null;
    archiveDigest: string;
    gradingStatus?: string | null;
    summaryJson?: JsonObject | null;
    lifecycleJson?: JsonObject | null;
    resultJson?: JsonObject | null;
    ownerKey?: string | null | undefined;
  }): Promise<LatchSubmissionRecord> {
    const existing = await this.submissionByDispatchId(input.dispatchId);
    if (existing) {
      if ((existing.owner_key ?? null) !== (input.ownerKey ?? null)) {
        throw new HttpError(409, "latch_submission_conflict", "Dispatch id is already registered to a different owner");
      }
      return existing;
    }

    const uploads = await this.sql`
      select upload_id, status, expected_digest, content_digest
      from ingest.uploads
      where upload_id = ${input.uploadId}
        and (${input.ownerKey ?? null}::text is null or owner_key = ${input.ownerKey ?? null})
      limit 1
    `;
    const upload = uploads[0] as { status: string; expected_digest: string | null; content_digest: string | null } | undefined;
    if (!upload) {
      throw new HttpError(404, "upload_not_found", "Upload not found");
    }
    if (upload.status !== "completed" || !upload.content_digest) {
      throw new HttpError(409, "upload_not_completed", "Upload must be completed before creating a latch submission");
    }
    if (upload.content_digest !== input.archiveDigest) {
      throw new HttpError(409, "upload_digest_mismatch", "Latch submission archive_digest must match the completed upload content digest", {
        archive_digest: input.archiveDigest,
        content_digest: upload.content_digest,
      });
    }

    const rows = await this.sql`
      insert into cloud.latch_submissions (
        dispatch_id,
        upload_id,
        owner_key,
        benchmark_ref,
        benchmark_digest,
        resolution_id,
        archive_digest,
        grading_status,
        summary_json,
        lifecycle_json,
        result_json
      )
      values (
        ${input.dispatchId},
        ${input.uploadId},
        ${input.ownerKey ?? null},
        ${input.benchmarkRef},
        ${input.benchmarkDigest ?? null},
        ${input.resolutionId ?? null},
        ${input.archiveDigest},
        ${input.gradingStatus ?? null},
        ${this.sql.json(input.summaryJson ?? {})},
        ${this.sql.json(input.lifecycleJson ?? {})},
        ${this.sql.json(input.resultJson ?? {})}
      )
      returning *
    `;
    return rows[0] as LatchSubmissionRecord;
  }

  async getSubmission(submissionId: string, ownerKey?: string): Promise<LatchSubmissionRecord | null> {
    const rows = await this.sql`
      select *
      from cloud.latch_submissions
      where submission_id = ${submissionId}
        and (${ownerKey ?? null}::text is null or owner_key = ${ownerKey ?? null})
      limit 1
    `;
    return (rows[0] as LatchSubmissionRecord | undefined) ?? null;
  }

  async listSubmissions(input?: { limit?: number; ownerKey?: string | undefined }): Promise<LatchSubmissionRecord[]> {
    const rows = await this.sql`
      select *
      from cloud.latch_submissions
      where (${input?.ownerKey ?? null}::text is null or owner_key = ${input?.ownerKey ?? null})
      order by created_at desc
      limit ${Math.max(1, Math.min(input?.limit ?? 50, 200))}
    `;
    return rows as unknown as LatchSubmissionRecord[];
  }

  private async submissionByDispatchId(dispatchId: string): Promise<LatchSubmissionRecord | null> {
    const rows = await this.sql`
      select *
      from cloud.latch_submissions
      where dispatch_id = ${dispatchId}
      limit 1
    `;
    return (rows[0] as LatchSubmissionRecord | undefined) ?? null;
  }
}
