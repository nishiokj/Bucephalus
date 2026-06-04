import { randomBytes } from "node:crypto";
import type { Sql } from "../db/client";
import { HttpError } from "../http";
import { sha256Digest, type JsonObject, type JsonValue } from "../primitives";
import type { ImportDiagnostic } from "../imports/sealedPackage";

export interface PackageArtifactRecord {
  package_digest: string;
  upload_id: string | null;
  storage_path: string | null;
  byte_size: number | null;
  media_type: string | null;
  manifest_json: JsonObject;
  resolved_experiment_json: JsonObject;
  target: JsonObject | null;
  image_refs: string[];
  diagnostics: ImportDiagnostic[];
  status: string;
  created_at: string;
  updated_at: string;
  owner_key?: string | null;
}

export class PackageRepository {
  constructor(private readonly sql: Sql) {}

  async upsertArtifact(input: {
    packageDigest: string;
    uploadId: string;
    storagePath: string;
    byteSize: number | null;
    mediaType: string;
    manifestJson: JsonObject;
    resolvedExperimentJson: JsonObject;
    target?: JsonObject | null;
    imageRefs: string[];
    diagnostics: ImportDiagnostic[];
    ownerKey?: string | null | undefined;
  }): Promise<PackageArtifactRecord> {
    return await this.sql.begin(async (tx) => {
      const rows = await tx`
        insert into cloud.package_artifacts (
          package_digest,
          upload_id,
          storage_path,
          byte_size,
          media_type,
          manifest_json,
          resolved_experiment_json,
          target,
          image_refs,
          diagnostics,
          status
        )
        values (
          ${input.packageDigest},
          ${input.uploadId},
          ${input.storagePath},
          ${input.byteSize},
          ${input.mediaType},
          ${this.sql.json(input.manifestJson)},
          ${this.sql.json(input.resolvedExperimentJson)},
          ${input.target ? this.sql.json(input.target) : null},
          ${this.sql.json(input.imageRefs as unknown as JsonValue[])},
          ${this.sql.json(input.diagnostics as unknown as JsonObject)},
          'accepted'
        )
        on conflict (package_digest) do update
        set upload_id = excluded.upload_id,
            storage_path = excluded.storage_path,
            byte_size = excluded.byte_size,
            media_type = excluded.media_type,
            manifest_json = excluded.manifest_json,
            resolved_experiment_json = excluded.resolved_experiment_json,
            target = excluded.target,
            image_refs = excluded.image_refs,
            diagnostics = excluded.diagnostics,
            status = excluded.status,
            updated_at = now()
        returning *
      `;
      if (input.ownerKey) {
        await tx`
          insert into cloud.package_artifact_owners (package_digest, owner_key, upload_id)
          values (${input.packageDigest}, ${input.ownerKey}, ${input.uploadId})
          on conflict (package_digest, owner_key) do update
          set upload_id = excluded.upload_id
        `;
      }
      return rows[0] as PackageArtifactRecord;
    });
  }

  async getArtifact(packageDigest: string, ownerKey?: string): Promise<PackageArtifactRecord | null> {
    const rows = await this.sql`
      select artifact.*
      from cloud.package_artifacts artifact
      where artifact.package_digest = ${packageDigest}
        and (
          ${ownerKey ?? null}::text is null
          or exists (
            select 1
            from cloud.package_artifact_owners owner
            where owner.package_digest = artifact.package_digest
              and owner.owner_key = ${ownerKey ?? null}
          )
        )
      limit 1
    `;
    return (rows[0] as PackageArtifactRecord | undefined) ?? null;
  }

  async listArtifacts(input?: { limit?: number; ownerKey?: string | undefined }): Promise<PackageArtifactRecord[]> {
    const rows = await this.sql`
      select artifact.*
      from cloud.package_artifacts artifact
      where (
        ${input?.ownerKey ?? null}::text is null
        or exists (
          select 1
          from cloud.package_artifact_owners owner
          where owner.package_digest = artifact.package_digest
            and owner.owner_key = ${input?.ownerKey ?? null}
        )
      )
      order by updated_at desc
      limit ${Math.max(1, Math.min(input?.limit ?? 50, 200))}
    `;
    return rows as unknown as PackageArtifactRecord[];
  }
}

export interface CloudRunRecord {
  run_id: string;
  package_digest: string;
  run_label: string | null;
  status: string;
  env: Record<string, string>;
  secret_refs: Record<string, string>;
  runtime_options: JsonObject;
  run_requirements: RunRequirements;
  created_at: string;
  updated_at: string;
  started_at: string | null;
  completed_at: string | null;
  error_message: string | null;
  owner_key?: string | null;
}

export interface RunAttemptRecord {
  attempt_id: string;
  run_id: string;
  worker_id: string;
  runner_instance_id: string | null;
  status: string;
  lease_expires_at: string;
  heartbeat_at: string;
  started_at: string;
  ended_at: string | null;
  error_message: string | null;
  created_at: string;
  updated_at: string;
  attempt_token?: string | null;
}

export interface RunEventRecord {
  event_id: string;
  run_id: string;
  attempt_id: string | null;
  seq: string | number;
  event_type: string;
  payload: JsonObject;
  created_at: string;
}

export interface RunRequirements {
  executor: "runner-docker" | "modal";
  requires: string[];
  image_refs: string[];
  secret_ids: string[];
  network_perimeter: RunNetworkPerimeter;
  sidecars: string[];
  accelerators: string[];
  arch: "x86_64" | "arm64";
  cpu_count: number;
  memory_mb: number;
  disk_mb: number;
  isolation: "reusable_vm" | "single_use_vm";
  timeout_ms: number | null;
  max_parallel_trials: number;
}

export interface RunNetworkPerimeter {
  default: "none";
  task_sandbox: "none";
  agent: "none";
  egress_hosts: string[];
}

export interface WorkerCapabilities {
  executors: string[];
  resources: string[];
  arch?: string | null;
  cpu_count?: number | null;
  memory_mb?: number | null;
  disk_mb?: number | null;
  isolation?: string[];
}

export class RunRepository {
  constructor(private readonly sql: Sql) {}

  async createRun(input: {
    packageDigest: string;
    runLabel?: string | null;
    env: Record<string, string>;
    secretRefs: Record<string, string>;
    runtimeOptions: JsonObject;
    runRequirements: RunRequirements;
    ownerKey?: string | null | undefined;
  }): Promise<CloudRunRecord> {
    return await this.sql.begin(async (tx) => {
      const rows = await tx`
        insert into cloud.runs (
          package_digest,
          run_label,
          env,
          secret_refs,
          runtime_options,
          run_requirements,
          status,
          owner_key
        )
        values (
          ${input.packageDigest},
          ${input.runLabel ?? null},
          ${this.sql.json(input.env as unknown as JsonObject)},
          ${this.sql.json(input.secretRefs as unknown as JsonObject)},
          ${this.sql.json(input.runtimeOptions)},
          ${this.sql.json(input.runRequirements as unknown as JsonObject)},
          'created',
          ${input.ownerKey ?? null}
        )
        returning *
      `.catch((error) => {
        if (isForeignKeyViolation(error)) {
          throw new HttpError(404, "package_not_found", "Package artifact not found");
        }
        throw error;
      });
      return rows[0] as CloudRunRecord;
    });
  }

  async getRun(runId: string, ownerKey?: string): Promise<CloudRunRecord | null> {
    const rows = await this.sql`
      select *
      from cloud.runs
      where run_id = ${runId}
        and (${ownerKey ?? null}::text is null or owner_key = ${ownerKey ?? null})
      limit 1
    `;
    return (rows[0] as CloudRunRecord | undefined) ?? null;
  }

  async listRuns(input?: { limit?: number; ownerKey?: string | undefined }): Promise<CloudRunRecord[]> {
    const rows = await this.sql`
      select *
      from cloud.runs
      where (${input?.ownerKey ?? null}::text is null or owner_key = ${input?.ownerKey ?? null})
      order by created_at desc
      limit ${Math.max(1, Math.min(input?.limit ?? 50, 200))}
    `;
    return rows as unknown as CloudRunRecord[];
  }

  async claimNextRun(input: {
    runnerInstanceId: string;
    leaseSeconds: number;
  }): Promise<{ run: CloudRunRecord; attempt: RunAttemptRecord } | null> {
    return await this.sql.begin(async (tx) => {
      const instances = await tx`
        select instance.*, pool.status as runner_pool_status
        from cloud.runner_instances instance
        join cloud.runner_pools pool using (runner_pool_id)
        where instance.runner_instance_id = ${input.runnerInstanceId}
          and instance.status = 'online'
          and pool.status = 'active'
          and instance.last_heartbeat_at >= now() - (${Math.max(input.leaseSeconds * 2, 30).toString()} || ' seconds')::interval
        for update
        limit 1
      `;
      const instance = instances[0] as { capabilities?: WorkerCapabilities } | undefined;
      if (!instance) {
        throw new HttpError(404, "runner_instance_not_claimable", "Runner instance is not online in an active pool");
      }
      const capabilities = normalizeCapabilities(instance.capabilities);
      const capabilityIsolation = capabilities.isolation ?? [];
      const candidates = await tx`
        select *
        from cloud.runs
        where status in ('created', 'waiting_for_runner')
          and run_requirements->>'executor' = any(${capabilities.executors})
          and (
            run_requirements->>'arch' is null
            or ${capabilities.arch ?? null}::text is null
            or run_requirements->>'arch' = ${capabilities.arch ?? null}
          )
          and (
            (run_requirements->>'cpu_count') is null
            or ${capabilities.cpu_count ?? null}::int is null
            or (run_requirements->>'cpu_count')::int <= ${capabilities.cpu_count ?? null}::int
          )
          and (
            (run_requirements->>'memory_mb') is null
            or ${capabilities.memory_mb ?? null}::int is null
            or (run_requirements->>'memory_mb')::int <= ${capabilities.memory_mb ?? null}::int
          )
          and (
            (run_requirements->>'disk_mb') is null
            or ${capabilities.disk_mb ?? null}::int is null
            or (run_requirements->>'disk_mb')::int <= ${capabilities.disk_mb ?? null}::int
          )
          and (
            run_requirements->>'isolation' is null
            or ${capabilityIsolation}::text[] = '{}'
            or run_requirements->>'isolation' = any(${capabilityIsolation})
          )
          and not exists (
            select 1
            from jsonb_array_elements_text(coalesce(run_requirements->'requires', '[]'::jsonb)) required(resource)
            where not (required.resource = any(${capabilities.resources}))
          )
        order by created_at asc
        for update skip locked
        limit 1
      `;
      const run = candidates[0] as CloudRunRecord | undefined;
      if (!run) {
        return null;
      }
      const attemptToken = randomAttemptToken();

      const attempts = await tx`
        insert into cloud.run_attempts (
          run_id,
          worker_id,
          runner_instance_id,
          lease_expires_at,
          status,
          attempt_token_hash
        )
        values (
          ${run.run_id},
          ${input.runnerInstanceId},
          ${input.runnerInstanceId},
          now() + (${input.leaseSeconds.toString()} || ' seconds')::interval,
          'running',
          ${attemptTokenHash(attemptToken)}
        )
        returning *
      `;
      const attempt = {
        ...(attempts[0] as RunAttemptRecord),
        attempt_token: attemptToken,
      };
      const updatedRuns = await tx`
        update cloud.runs
        set status = 'running',
            started_at = coalesce(started_at, now()),
            updated_at = now(),
            error_message = null
        where run_id = ${run.run_id}
        returning *
      `;
      return {
        run: updatedRuns[0] as CloudRunRecord,
        attempt,
      };
    });
  }

  async heartbeatAttempt(input: {
    attemptId: string;
    runnerInstanceId: string;
    leaseSeconds: number;
  }): Promise<RunAttemptRecord> {
    const rows = await this.sql`
      update cloud.run_attempts
      set heartbeat_at = now(),
          lease_expires_at = now() + (${input.leaseSeconds.toString()} || ' seconds')::interval,
          updated_at = now()
      where attempt_id = ${input.attemptId}
        and runner_instance_id = ${input.runnerInstanceId}
        and status = 'running'
        and lease_expires_at >= now()
      returning *
    `;
    const attempt = rows[0] as RunAttemptRecord | undefined;
    if (!attempt) {
      throw new HttpError(404, "attempt_not_found", "Active run attempt not found for worker");
    }
    return attempt;
  }

  async appendRunEvent(input: {
    attemptId: string;
    runnerInstanceId: string;
    eventType: string;
    payload: JsonObject;
  }): Promise<RunEventRecord> {
    const rows = await this.sql`
      insert into cloud.run_events (
        run_id,
        attempt_id,
        seq,
        event_type,
        payload
      )
      select
        active_attempt.run_id,
        active_attempt.attempt_id,
        coalesce((select max(seq) + 1 from cloud.run_events where run_id = active_attempt.run_id), 1),
        ${input.eventType},
        ${this.sql.json(input.payload)}
      from (
        select run_id, attempt_id
        from cloud.run_attempts
        where attempt_id = ${input.attemptId}
          and runner_instance_id = ${input.runnerInstanceId}
          and status = 'running'
          and lease_expires_at >= now()
        limit 1
      ) active_attempt
      returning *
    `;
    const event = rows[0] as RunEventRecord | undefined;
    if (!event) {
      throw new HttpError(404, "attempt_not_found", "Active unexpired run attempt not found for worker");
    }
    return event;
  }

  async completeAttempt(input: {
    attemptId: string;
    runnerInstanceId: string;
  }): Promise<{ run: CloudRunRecord; attempt: RunAttemptRecord }> {
    return await this.sql.begin(async (tx) => {
      const attempts = await tx`
        update cloud.run_attempts
        set status = 'completed',
            ended_at = now(),
            updated_at = now()
        where attempt_id = ${input.attemptId}
          and runner_instance_id = ${input.runnerInstanceId}
          and status = 'running'
          and lease_expires_at >= now()
        returning *
      `;
      const attempt = attempts[0] as RunAttemptRecord | undefined;
      if (!attempt) {
        throw new HttpError(404, "attempt_not_found", "Active run attempt not found for worker");
      }
      const runs = await tx`
        update cloud.runs
        set status = 'completed',
            completed_at = now(),
            updated_at = now(),
            error_message = null
        where run_id = ${attempt.run_id}
          and status = 'running'
        returning *
      `;
      const run = runs[0] as CloudRunRecord | undefined;
      if (!run) {
        throw new HttpError(409, "run_not_running", "Run is no longer running");
      }
      return {
        run,
        attempt,
      };
    });
  }

  async failAttempt(input: {
    attemptId: string;
    runnerInstanceId: string;
    message: string;
  }): Promise<{ run: CloudRunRecord; attempt: RunAttemptRecord }> {
    return await this.sql.begin(async (tx) => {
      const attempts = await tx`
        update cloud.run_attempts
        set status = 'failed',
            ended_at = now(),
            error_message = ${input.message},
            updated_at = now()
        where attempt_id = ${input.attemptId}
          and runner_instance_id = ${input.runnerInstanceId}
          and status = 'running'
          and lease_expires_at >= now()
        returning *
      `;
      const attempt = attempts[0] as RunAttemptRecord | undefined;
      if (!attempt) {
        throw new HttpError(404, "attempt_not_found", "Active run attempt not found for worker");
      }
      const runs = await tx`
        update cloud.runs
        set status = 'failed',
            completed_at = now(),
            updated_at = now(),
            error_message = ${input.message}
        where run_id = ${attempt.run_id}
          and status = 'running'
        returning *
      `;
      const run = runs[0] as CloudRunRecord | undefined;
      if (!run) {
        throw new HttpError(409, "run_not_running", "Run is no longer running");
      }
      return {
        run,
        attempt,
      };
    });
  }

  async expireLeases(): Promise<Array<{ run: CloudRunRecord; attempt: RunAttemptRecord }>> {
    return await this.sql.begin(async (tx) => {
      const attempts = await tx`
        update cloud.run_attempts
        set status = 'expired',
            ended_at = now(),
            error_message = 'worker lease expired',
            updated_at = now()
        where status = 'running'
          and lease_expires_at < now()
        returning *
      `;
      const expired: Array<{ run: CloudRunRecord; attempt: RunAttemptRecord }> = [];
      for (const attempt of attempts as unknown as RunAttemptRecord[]) {
        const runs = await tx`
          update cloud.runs
          set status = 'waiting_for_runner',
              updated_at = now(),
              error_message = 'previous worker lease expired'
          where run_id = ${attempt.run_id}
            and status = 'running'
          returning *
        `;
        const run = runs[0] as CloudRunRecord | undefined;
        if (run) {
          expired.push({ run, attempt });
        }
      }
      return expired;
    });
  }

  async verifyAttemptToken(input: {
    attemptId: string;
    token: string;
    runnerInstanceId?: string | null;
    packageDigest?: string | null;
  }): Promise<void> {
    const rows = await this.sql`
      select attempt.attempt_id
      from cloud.run_attempts attempt
      join cloud.runs run using (run_id)
      where attempt.attempt_id = ${input.attemptId}
        and attempt.status = 'running'
        and attempt.lease_expires_at >= now()
        and attempt.attempt_token_hash = ${attemptTokenHash(input.token)}
        and (${input.runnerInstanceId ?? null}::text is null or attempt.runner_instance_id = ${input.runnerInstanceId ?? null})
        and (${input.packageDigest ?? null}::text is null or run.package_digest = ${input.packageDigest ?? null})
      limit 1
    `;
    if (rows.length === 0) {
      throw new HttpError(401, "unauthorized", "worker attempt requires a valid attempt token");
    }
  }
}

export function requireStringMap(value: unknown, pointer: string): Record<string, string> {
  if (!isRecord(value)) {
    return {};
  }
  const out: Record<string, string> = {};
  for (const [key, item] of Object.entries(value)) {
    if (typeof item !== "string") {
      throw new HttpError(400, "invalid_string_map", `${pointer}/${escapePointer(key)} must be a string`);
    }
    out[key] = item;
  }
  return out;
}

export function optionalJsonObject(value: JsonValue | undefined, pointer: string): JsonObject {
  if (value === undefined) {
    return {};
  }
  if (!isRecord(value)) {
    throw new HttpError(400, "invalid_json_object", `${pointer} must be a JSON object`);
  }
  return value as JsonObject;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isForeignKeyViolation(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    "code" in error &&
    (error as { code?: unknown }).code === "23503"
  );
}

function escapePointer(value: string): string {
  return value.replaceAll("~", "~0").replaceAll("/", "~1");
}

function normalizeCapabilities(value: unknown): WorkerCapabilities {
  if (!isRecord(value)) {
    return { executors: [], resources: [] };
  }
  return {
    executors: stringArray(value.executors),
    resources: stringArray(value.resources),
    arch: typeof value.arch === "string" ? value.arch : null,
    cpu_count: optionalPositiveInt(value.cpu_count),
    memory_mb: optionalPositiveInt(value.memory_mb),
    disk_mb: optionalPositiveInt(value.disk_mb),
    isolation: stringArray(value.isolation),
  };
}

function randomAttemptToken(): string {
  return randomBytes(32).toString("base64url");
}

function attemptTokenHash(token: string): string {
  return sha256Digest(token);
}

function stringArray(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.filter((item): item is string => typeof item === "string" && item.trim().length > 0);
}

function optionalPositiveInt(value: unknown): number | null {
  if (typeof value === "number" && Number.isInteger(value) && value > 0) {
    return value;
  }
  if (typeof value === "string" && /^\d+$/.test(value)) {
    const parsed = Number.parseInt(value, 10);
    return parsed > 0 ? parsed : null;
  }
  return null;
}
