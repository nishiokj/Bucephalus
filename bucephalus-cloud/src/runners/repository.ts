import type { Sql } from "../db/client";
import { HttpError } from "../http";
import type { JsonObject } from "../primitives";
import type { WorkerCapabilities } from "../packages/repository";

export interface RunnerPoolRecord {
  runner_pool_id: string;
  name: string;
  status: "active" | "draining" | "disabled";
  active_worker_image_id: string | null;
  capabilities: WorkerCapabilities;
  metadata: JsonObject;
  created_at: string;
  updated_at: string;
}

export interface RunnerWorkerImageRecord {
  runner_worker_image_id: string;
  image_ref: string;
  registry_host: string;
  repository: string;
  digest: string;
  release_version: string | null;
  release_git_sha: string | null;
  promotion_evidence_uri: string | null;
  promotion_evidence_sha256: string | null;
  modal_launcher_sha256: string | null;
  worker_runner_sha256: string | null;
  boundary_verified_at: string | null;
  metadata: JsonObject;
  created_at: string;
}

export interface RunnerInstanceRecord {
  runner_instance_id: string;
  runner_pool_id: string;
  instance_name: string;
  status: RunnerInstanceStatus;
  capabilities: WorkerCapabilities;
  metadata: JsonObject;
  last_heartbeat_at: string;
  created_at: string;
  updated_at: string;
}

export type RunnerInstanceStatus = "online" | "draining" | "offline" | "unhealthy";

export interface RunnerProvisionRequestRecord {
  provision_request_id: string;
  runner_pool_id: string;
  run_id: string | null;
  status: "requested" | "provisioning" | "active" | "failed" | "reaped";
  provider: string;
  provider_instance_id: string | null;
  instance_name: string | null;
  runner_instance_id: string | null;
  requirements: JsonObject;
  metadata: JsonObject;
  error_message: string | null;
  created_at: string;
  updated_at: string;
}

export interface ReapableRunnerProvisionRequestRecord extends RunnerProvisionRequestRecord {
  runner_instance_status: RunnerInstanceStatus | null;
  runner_instance_metadata: JsonObject | null;
  run_status?: string | null;
}

export interface QueuedRunDemandRecord {
  run_id: string;
  run_requirements: {
    executor: string;
    requires: string[];
    image_refs: string[];
    arch?: string;
    cpu_count?: number;
    memory_mb?: number;
    disk_mb?: number;
    isolation?: string;
    timeout_ms?: number | null;
    max_parallel_trials?: number;
  };
  created_at: string;
}

export class RunnerRepository {
  constructor(private readonly sql: Sql) {}

  async createPool(input: {
    name: string;
    capabilities: WorkerCapabilities;
    metadata: JsonObject;
  }): Promise<RunnerPoolRecord> {
    const rows = await this.sql`
      insert into cloud.runner_pools (
        name,
        capabilities,
        metadata
      )
      values (
        ${input.name},
        ${this.sql.json(input.capabilities as unknown as JsonObject)},
        ${this.sql.json(input.metadata)}
      )
      returning *
    `;
    return rows[0] as RunnerPoolRecord;
  }

  async listPools(): Promise<RunnerPoolRecord[]> {
    const rows = await this.sql`
      select *
      from cloud.runner_pools
      order by created_at asc
    `;
    return rows as unknown as RunnerPoolRecord[];
  }

  async getPool(poolId: string): Promise<RunnerPoolRecord | null> {
    const rows = await this.sql`
      select *
      from cloud.runner_pools
      where runner_pool_id = ${poolId}
      limit 1
    `;
    return (rows[0] as RunnerPoolRecord | undefined) ?? null;
  }

  async setPoolStatus(input: {
    poolId: string;
    status: "active" | "draining" | "disabled";
  }): Promise<RunnerPoolRecord> {
    const rows = await this.sql`
      update cloud.runner_pools
      set status = ${input.status},
          updated_at = now()
      where runner_pool_id = ${input.poolId}
      returning *
    `;
    const pool = rows[0] as RunnerPoolRecord | undefined;
    if (!pool) {
      throw new HttpError(404, "runner_pool_not_found", "Runner pool not found");
    }
    return pool;
  }

  async setPoolCapabilities(input: {
    poolId: string;
    capabilities: WorkerCapabilities;
  }): Promise<RunnerPoolRecord> {
    const rows = await this.sql`
      update cloud.runner_pools
      set capabilities = ${this.sql.json(input.capabilities as unknown as JsonObject)},
          updated_at = now()
      where runner_pool_id = ${input.poolId}
      returning *
    `;
    const pool = rows[0] as RunnerPoolRecord | undefined;
    if (!pool) {
      throw new HttpError(404, "runner_pool_not_found", "Runner pool not found");
    }
    return pool;
  }

  async getActiveWorkerImageForPool(poolId: string): Promise<RunnerWorkerImageRecord | null> {
    const rows = await this.sql`
      select image.*
      from cloud.runner_pools pool
      join cloud.runner_worker_images image
        on image.runner_worker_image_id = pool.active_worker_image_id
      where pool.runner_pool_id = ${poolId}
      limit 1
    `;
    return (rows[0] as RunnerWorkerImageRecord | undefined) ?? null;
  }

  async promoteWorkerImage(input: {
    poolId: string;
    imageRef: string;
    registryHost: string;
    repository: string;
    digest: string;
    releaseVersion?: string | null;
    releaseGitSha?: string | null;
    promotionEvidenceUri?: string | null;
    promotionEvidenceSha256?: string | null;
    modalLauncherSha256?: string | null;
    workerRunnerSha256?: string | null;
    boundaryVerifiedAt?: string | null;
    metadata?: JsonObject;
  }): Promise<{ pool: RunnerPoolRecord; workerImage: RunnerWorkerImageRecord }> {
    return await this.sql.begin(async (tx) => {
      const imageRows = await tx`
        insert into cloud.runner_worker_images (
          image_ref,
          registry_host,
          repository,
          digest,
          release_version,
          release_git_sha,
          promotion_evidence_uri,
          promotion_evidence_sha256,
          modal_launcher_sha256,
          worker_runner_sha256,
          boundary_verified_at,
          metadata
        )
        values (
          ${input.imageRef},
          ${input.registryHost},
          ${input.repository},
          ${input.digest},
          ${input.releaseVersion ?? null},
          ${input.releaseGitSha ?? null},
          ${input.promotionEvidenceUri ?? null},
          ${input.promotionEvidenceSha256 ?? null},
          ${input.modalLauncherSha256 ?? null},
          ${input.workerRunnerSha256 ?? null},
          ${input.boundaryVerifiedAt ?? null},
          ${this.sql.json(input.metadata ?? {})}
        )
        on conflict (image_ref) do update
        set release_version = excluded.release_version,
            release_git_sha = excluded.release_git_sha,
            promotion_evidence_uri = excluded.promotion_evidence_uri,
            promotion_evidence_sha256 = excluded.promotion_evidence_sha256,
            modal_launcher_sha256 = excluded.modal_launcher_sha256,
            worker_runner_sha256 = excluded.worker_runner_sha256,
            boundary_verified_at = excluded.boundary_verified_at,
            metadata = cloud.runner_worker_images.metadata || excluded.metadata
        returning *
      `;
      const workerImage = imageRows[0] as RunnerWorkerImageRecord;

      const poolRows = await tx`
        update cloud.runner_pools
        set active_worker_image_id = ${workerImage.runner_worker_image_id},
            updated_at = now()
        where runner_pool_id = ${input.poolId}
        returning *
      `;
      const pool = poolRows[0] as RunnerPoolRecord | undefined;
      if (!pool) {
        throw new HttpError(404, "runner_pool_not_found", "Runner pool not found");
      }
      return { pool, workerImage };
    });
  }

  async registerInstance(input: {
    runnerPoolId: string;
    instanceName: string;
    capabilities: WorkerCapabilities;
    metadata: JsonObject;
  }): Promise<RunnerInstanceRecord> {
    const provisionRequestId = typeof input.metadata.provision_request_id === "string"
      ? input.metadata.provision_request_id
      : null;
    return await this.sql.begin(async (tx) => {
      const pools = await tx`
        select *
        from cloud.runner_pools
        where runner_pool_id = ${input.runnerPoolId}
        for update
        limit 1
      `;
      const pool = pools[0] as RunnerPoolRecord | undefined;
      if (!pool) {
        throw new HttpError(404, "runner_pool_not_found", "Runner pool not found");
      }
      if (pool.status === "disabled") {
        throw new HttpError(409, "runner_pool_disabled", "Runner pool is disabled");
      }

      if (provisionRequestId) {
        const requests = await tx`
          select *
          from cloud.runner_provision_requests
          where provision_request_id = ${provisionRequestId}
            and runner_pool_id = ${input.runnerPoolId}
          for update
          limit 1
        `;
        const request = requests[0] as RunnerProvisionRequestRecord | undefined;
        if (!request) {
          throw new HttpError(404, "provision_request_not_found", "Provision request not found");
        }
        if (request.status !== "requested" && request.status !== "provisioning") {
          throw new HttpError(409, "provision_request_closed", "Provision request is no longer open");
        }
      }

      const rows = await tx`
        insert into cloud.runner_instances (
          runner_pool_id,
          instance_name,
          status,
          capabilities,
          metadata
        )
        values (
          ${input.runnerPoolId},
          ${input.instanceName},
          'online',
          ${this.sql.json(input.capabilities as unknown as JsonObject)},
          ${this.sql.json(input.metadata)}
        )
        returning *
      `;
      const instance = rows[0] as RunnerInstanceRecord;
      if (provisionRequestId) {
        await tx`
          update cloud.runner_provision_requests
          set status = 'active',
              runner_instance_id = ${instance.runner_instance_id},
              provider_instance_id = coalesce(provider_instance_id, ${typeof input.metadata.provider_instance_id === "string" ? input.metadata.provider_instance_id : null}),
              instance_name = coalesce(instance_name, ${input.instanceName}),
              updated_at = now()
          where provision_request_id = ${provisionRequestId}
            and runner_pool_id = ${input.runnerPoolId}
            and status in ('requested', 'provisioning')
        `;
      }
      return instance;
    });
  }

  async heartbeatInstance(input: {
    runnerInstanceId: string;
    capabilities?: WorkerCapabilities;
    metadata?: JsonObject;
  }): Promise<RunnerInstanceRecord> {
    const rows = input.capabilities || input.metadata
      ? await this.sql`
          update cloud.runner_instances
          set status = case when status = 'offline' then 'online'::cloud.runner_instance_status else status end,
              capabilities = coalesce(${input.capabilities ? this.sql.json(input.capabilities as unknown as JsonObject) : null}, capabilities),
              metadata = case
                when ${input.metadata ? this.sql.json(input.metadata) : null} is null then metadata
                else metadata || ${input.metadata ? this.sql.json(input.metadata) : null}::jsonb
              end,
              last_heartbeat_at = now(),
              updated_at = now()
          where runner_instance_id = ${input.runnerInstanceId}
          returning *
        `
      : await this.sql`
          update cloud.runner_instances
          set status = case when status = 'offline' then 'online'::cloud.runner_instance_status else status end,
              last_heartbeat_at = now(),
              updated_at = now()
          where runner_instance_id = ${input.runnerInstanceId}
          returning *
        `;
    const instance = rows[0] as RunnerInstanceRecord | undefined;
    if (!instance) {
      throw new HttpError(404, "runner_instance_not_found", "Runner instance not found");
    }
    return instance;
  }

  async setInstanceStatus(input: {
    runnerInstanceId: string;
    status: RunnerInstanceStatus;
    metadataPatch?: JsonObject;
  }): Promise<RunnerInstanceRecord> {
    const rows = input.metadataPatch
      ? await this.sql`
          update cloud.runner_instances
          set status = ${input.status},
              metadata = metadata || ${this.sql.json(input.metadataPatch)}::jsonb,
              updated_at = now()
          where runner_instance_id = ${input.runnerInstanceId}
          returning *
        `
      : await this.sql`
          update cloud.runner_instances
          set status = ${input.status},
              updated_at = now()
          where runner_instance_id = ${input.runnerInstanceId}
          returning *
        `;
    const instance = rows[0] as RunnerInstanceRecord | undefined;
    if (!instance) {
      throw new HttpError(404, "runner_instance_not_found", "Runner instance not found");
    }
    return instance;
  }

  async markStaleInstancesOffline(input: {
    runnerPoolId?: string;
    staleAfterSeconds: number;
  }): Promise<RunnerInstanceRecord[]> {
    const rows = await this.sql`
      update cloud.runner_instances
      set status = 'offline',
          metadata = metadata || ${this.sql.json({
            last_offline: {
              reason: "heartbeat_stale",
              recorded_at: new Date().toISOString(),
            },
          })}::jsonb,
          updated_at = now()
      where status in ('online', 'draining')
        and (${input.runnerPoolId ?? null}::uuid is null or runner_pool_id = ${input.runnerPoolId ?? null})
        and last_heartbeat_at < now() - (${input.staleAfterSeconds.toString()} || ' seconds')::interval
      returning *
    `;
    return rows as unknown as RunnerInstanceRecord[];
  }

  async listQueuedDemand(input: {
    limit: number;
  }): Promise<QueuedRunDemandRecord[]> {
    const rows = await this.sql`
      select run_id, run_requirements, created_at
      from cloud.runs
      where status in ('created', 'waiting_for_runner')
      order by created_at asc
      limit ${input.limit}
    `;
    return rows as unknown as QueuedRunDemandRecord[];
  }

  async listClaimableInstances(input: {
    runnerPoolId?: string;
    staleAfterSeconds: number;
  }): Promise<RunnerInstanceRecord[]> {
    const rows = await this.sql`
      select instance.*
      from cloud.runner_instances instance
      join cloud.runner_pools pool using (runner_pool_id)
      where instance.status = 'online'
        and pool.status = 'active'
        and (${input.runnerPoolId ?? null}::uuid is null or instance.runner_pool_id = ${input.runnerPoolId ?? null})
        and instance.last_heartbeat_at >= now() - (${input.staleAfterSeconds.toString()} || ' seconds')::interval
        and not exists (
          select 1
          from cloud.run_attempts attempt
          where attempt.runner_instance_id = instance.runner_instance_id
            and attempt.status = 'running'
        )
      order by instance.created_at asc
    `;
    return rows as unknown as RunnerInstanceRecord[];
  }

  async listInstances(input?: {
    runnerPoolId?: string;
    limit?: number;
  }): Promise<RunnerInstanceRecord[]> {
    const rows = await this.sql`
      select *
      from cloud.runner_instances
      where (${input?.runnerPoolId ?? null}::uuid is null or runner_pool_id = ${input?.runnerPoolId ?? null})
      order by updated_at desc
      limit ${Math.max(1, Math.min(input?.limit ?? 100, 500))}
    `;
    return rows as unknown as RunnerInstanceRecord[];
  }

  async listOpenProvisionRequests(input?: {
    runnerPoolId?: string;
  }): Promise<RunnerProvisionRequestRecord[]> {
    const rows = await this.sql`
      select *
      from cloud.runner_provision_requests
      where status in ('requested', 'provisioning')
        and (${input?.runnerPoolId ?? null}::uuid is null or runner_pool_id = ${input?.runnerPoolId ?? null})
      order by created_at asc
    `;
    return rows as unknown as RunnerProvisionRequestRecord[];
  }

  async listProvisionRequests(input?: {
    runnerPoolId?: string;
    limit?: number;
  }): Promise<RunnerProvisionRequestRecord[]> {
    const rows = await this.sql`
      select *
      from cloud.runner_provision_requests
      where (${input?.runnerPoolId ?? null}::uuid is null or runner_pool_id = ${input?.runnerPoolId ?? null})
      order by updated_at desc
      limit ${Math.max(1, Math.min(input?.limit ?? 100, 500))}
    `;
    return rows as unknown as RunnerProvisionRequestRecord[];
  }

  async listReapableProvisionRequests(input: {
    runnerPoolId: string;
    provisioningTimeoutSeconds: number;
    limit: number;
  }): Promise<ReapableRunnerProvisionRequestRecord[]> {
    const rows = await this.sql`
      select provision.*,
             instance.status as runner_instance_status,
             instance.metadata as runner_instance_metadata
      from cloud.runner_provision_requests provision
      left join cloud.runner_instances instance
        on instance.runner_instance_id = provision.runner_instance_id
      where provision.runner_pool_id = ${input.runnerPoolId}
        and provision.provider_instance_id is not null
        and (
          provision.status = 'failed'
          or (
            provision.status = 'provisioning'
            and provision.updated_at < now() - (${input.provisioningTimeoutSeconds.toString()} || ' seconds')::interval
          )
          or (
            provision.status = 'active'
            and instance.status in ('offline', 'unhealthy')
          )
        )
      order by provision.updated_at asc
      limit ${input.limit}
    `;
    return rows as unknown as ReapableRunnerProvisionRequestRecord[];
  }

  async listIdleCompletedRunProvisionRequests(input: {
    runnerPoolId: string;
    idleReapDelaySeconds: number;
    limit: number;
  }): Promise<ReapableRunnerProvisionRequestRecord[]> {
    const rows = await this.sql`
      select provision.*,
             instance.status as runner_instance_status,
             instance.metadata as runner_instance_metadata,
             run.status as run_status
      from cloud.runner_provision_requests provision
      join cloud.runs run
        on run.run_id = provision.run_id
      left join cloud.runner_instances instance
        on instance.runner_instance_id = provision.runner_instance_id
      where provision.runner_pool_id = ${input.runnerPoolId}
        and provision.status = 'active'
        and provision.provider_instance_id is not null
        and run.status in ('completed', 'failed', 'cancelled')
        and run.updated_at <= now() - (${Math.max(0, input.idleReapDelaySeconds).toString()} || ' seconds')::interval
        and not exists (
          select 1
          from cloud.run_attempts attempt
          where attempt.runner_instance_id = provision.runner_instance_id
            and attempt.status = 'running'
        )
      order by run.updated_at asc
      limit ${input.limit}
    `;
    return rows as unknown as ReapableRunnerProvisionRequestRecord[];
  }

  async failStaleUnacceptedProvisionRequests(input: {
    runnerPoolId: string;
    provisioningTimeoutSeconds: number;
  }): Promise<RunnerProvisionRequestRecord[]> {
    const rows = await this.sql`
      update cloud.runner_provision_requests
      set status = 'failed',
          error_message = coalesce(error_message, 'provision request timed out before provider instance id was recorded'),
          metadata = metadata || ${this.sql.json({
            failed_at: new Date().toISOString(),
            failure_reason: "provision_request_unaccepted_timeout",
          })}::jsonb,
          updated_at = now()
      where runner_pool_id = ${input.runnerPoolId}
        and status in ('requested', 'provisioning')
        and provider_instance_id is null
        and updated_at < now() - (${input.provisioningTimeoutSeconds.toString()} || ' seconds')::interval
      returning *
    `;
    return rows as unknown as RunnerProvisionRequestRecord[];
  }

  async createProvisionRequest(input: {
    runnerPoolId: string;
    runId: string;
    provider: string;
    requirements: JsonObject;
    metadata: JsonObject;
  }): Promise<RunnerProvisionRequestRecord | null> {
    const rows = await this.sql`
      insert into cloud.runner_provision_requests (
        runner_pool_id,
        run_id,
        provider,
        requirements,
        metadata
      )
      values (
        ${input.runnerPoolId},
        ${input.runId},
        ${input.provider},
        ${this.sql.json(input.requirements)},
        ${this.sql.json(input.metadata)}
      )
      on conflict do nothing
      returning *
    `;
    return (rows[0] as RunnerProvisionRequestRecord | undefined) ?? null;
  }

  async markProvisioning(input: {
    provisionRequestId: string;
    providerInstanceId?: string | null;
    instanceName?: string | null;
    metadata?: JsonObject;
  }): Promise<RunnerProvisionRequestRecord> {
    const rows = await this.sql`
      update cloud.runner_provision_requests
      set status = case
            when status = 'active' then status
            else 'provisioning'::cloud.runner_provision_status
          end,
          provider_instance_id = coalesce(${input.providerInstanceId ?? null}, provider_instance_id),
          instance_name = coalesce(${input.instanceName ?? null}, instance_name),
          metadata = metadata || ${this.sql.json(input.metadata ?? {})}::jsonb,
          updated_at = now()
      where provision_request_id = ${input.provisionRequestId}
        and status in ('requested', 'provisioning', 'active')
      returning *
    `;
    const request = rows[0] as RunnerProvisionRequestRecord | undefined;
    if (!request) {
      throw new HttpError(404, "provision_request_not_found", "Open provision request not found");
    }
    return request;
  }

  async failProvisionRequest(input: {
    provisionRequestId: string;
    message: string;
    metadata?: JsonObject;
  }): Promise<RunnerProvisionRequestRecord> {
    const rows = await this.sql`
      update cloud.runner_provision_requests
      set status = case
            when status = 'active' then status
            else 'failed'::cloud.runner_provision_status
          end,
          error_message = ${input.message},
          metadata = metadata || ${this.sql.json(input.metadata ?? {})}::jsonb,
          updated_at = now()
      where provision_request_id = ${input.provisionRequestId}
        and status in ('requested', 'provisioning', 'active', 'failed')
      returning *
    `;
    const request = rows[0] as RunnerProvisionRequestRecord | undefined;
    if (!request) {
      throw new HttpError(404, "provision_request_not_found", "Provision request not found");
    }
    return request;
  }

  async markProvisionRequestReaped(input: {
    provisionRequestId: string;
    metadata?: JsonObject;
  }): Promise<RunnerProvisionRequestRecord> {
    const rows = await this.sql`
      update cloud.runner_provision_requests
      set status = 'reaped',
          metadata = metadata || ${this.sql.json(input.metadata ?? {})}::jsonb,
          updated_at = now()
      where provision_request_id = ${input.provisionRequestId}
        and status in ('provisioning', 'active', 'failed')
      returning *
    `;
    const request = rows[0] as RunnerProvisionRequestRecord | undefined;
    if (!request) {
      throw new HttpError(404, "provision_request_not_found", "Reapable provision request not found");
    }
    return request;
  }
}
